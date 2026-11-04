use clap::Parser;
use poc_server::{
    MAX_FRAME,
    game::{Client, Game},
    macho::{self, Packet},
    pickle, read_frame,
    state::{Error, Result, State, hash_password},
    write_frame,
};
use rand::RngCore;
use std::{
    collections::BTreeMap,
    io,
    net::{Shutdown, TcpListener, TcpStream},
    sync::mpsc::{self, SyncSender},
    thread,
    time::Duration,
};

#[derive(Parser)]
#[command(
    about = "Native Rust MachoNet server for EVE Gemini build 665",
    version
)]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value_t = 26000)]
    port: u16,
    #[arg(long)]
    closed: bool,
    #[arg(short = 'v', long)]
    verbose: bool,
    #[arg(long, help = "PostgreSQL connection string; defaults to DATABASE_URL")]
    database_url: Option<String>,
    #[arg(
        long,
        conflicts_with = "check_db",
        help = "Initialize NPC market and verify the PostgreSQL database"
    )]
    setup_db: bool,
    #[arg(long, help = "Read-only database consistency checks, then exit")]
    check_db: bool,
    #[arg(long, help = "Check the local game listener, then exit")]
    health_check: bool,
    #[arg(long)]
    accounts: bool,
    #[arg(long)]
    summary: bool,
    #[arg(long,num_args=2,value_names=["USERNAME","PASSWORD"])]
    add_account: Option<Vec<String>>,
    #[arg(long,num_args=2,value_names=["USERNAME","PASSWORD"])]
    set_password: Option<Vec<String>>,
    #[arg(long)]
    ban: Option<String>,
    #[arg(long)]
    unban: Option<String>,
    #[arg(long,num_args=2,value_names=["CHARACTER_ID","USERNAME"])]
    assign: Option<Vec<String>>,
    #[arg(long)]
    delete_account: Option<String>,
}
enum Event {
    Connect(i64, SyncSender<Vec<u8>>, TcpStream),
    Packet(i64, Box<Packet>),
    Disconnect(i64),
}
struct Peer {
    sender: SyncSender<Vec<u8>>,
    socket: TcpStream,
}
fn io_error(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn connection(mut socket: TcpStream, id: i64, events: SyncSender<Event>) -> io::Result<()> {
    // Winsock can inherit the listener's nonblocking mode on accepted sockets.
    socket.set_nonblocking(false)?;
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(15)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    write_frame(
        &mut socket,
        &pickle::dumps(&macho::challenge()).map_err(io_error)?,
        MAX_FRAME,
    )?;
    let reply = pickle::loads(&read_frame(&mut socket, MAX_FRAME)?).map_err(io_error)?;
    if reply.seq() != macho::challenge().seq() {
        return Err(io_error("MachoNet version mismatch"));
    }
    socket.set_read_timeout(None)?;
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(64);
    let mut writer = socket.try_clone()?;
    let notifications = events.clone();
    events
        .send(Event::Connect(id, tx, socket.try_clone()?))
        .map_err(|_| io_error("server stopped"))?;
    thread::spawn(move || {
        while let Ok(frame) = rx.recv() {
            if write_frame(&mut writer, &frame, MAX_FRAME).is_err() {
                break;
            }
        }
        let _ = writer.shutdown(Shutdown::Both);
        let _ = notifications.send(Event::Disconnect(id));
    });
    let result = (|| {
        loop {
            let frame = read_frame(&mut socket, MAX_FRAME)?;
            let packet = Packet::decode(&frame).map_err(io_error)?;
            events
                .send(Event::Packet(id, Box::new(packet)))
                .map_err(|_| io_error("server stopped"))?;
        }
    })();
    let _ = socket.shutdown(Shutdown::Both);
    let _ = events.send(Event::Disconnect(id));
    result
}

fn serve(args: &Args, mut state: State) -> std::result::Result<(), String> {
    state.lock_server().map_err(|e| format!("{e:?}"))?;
    // Bind before boot writes so a second launch cannot alter an active server's
    // order book and then discover that its listen port was already occupied.
    let listener = TcpListener::bind((args.host.as_str(), args.port)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    state.boot_market().map_err(|e| format!("{e:?}"))?;
    let mut game = Game::new(state, !args.closed);
    let mut peers = BTreeMap::<i64, Peer>::new();
    let (tx, rx) = mpsc::sync_channel::<Event>(256);
    let mut next_id = 1;
    eprintln!(
        "MachoNet Rust server listening on {} (version {})",
        listener.local_addr().unwrap(),
        macho::VERSION
    );
    loop {
        // Cap accepts per iteration so a connect storm cannot starve gameplay.
        for _ in 0..16 {
            match listener.accept() {
                Ok((socket, peer)) => {
                    let id = next_id;
                    next_id += 1;
                    let tx = tx.clone();
                    if args.verbose {
                        eprintln!("client {id} connected from {peer}");
                    }
                    thread::spawn(move || {
                        if let Err(e) = connection(socket, id, tx)
                            && !matches!(
                                e.kind(),
                                io::ErrorKind::UnexpectedEof
                                    | io::ErrorKind::ConnectionReset
                                    | io::ErrorKind::ConnectionAborted
                            )
                        {
                            eprintln!("client {id}: {e}");
                        }
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.to_string()),
            }
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(Event::Connect(id, sender, socket)) => {
                peers.insert(id, Peer { sender, socket });
                game.clients.insert(id, Client::new(id));
            }
            Ok(Event::Packet(id, packet)) => {
                if args.verbose {
                    eprintln!("client {id}: command {}", packet.command);
                }
                game.handle(id, *packet);
            }
            Ok(Event::Disconnect(id)) => {
                if let Some(p) = peers.remove(&id) {
                    let _ = p.socket.shutdown(Shutdown::Both);
                }
                game.disconnect(id);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(_) => break,
        }
        if let Err(e) = game.tick() {
            eprintln!("simulation tick: {e:?}");
        }
        let mut closed = vec![];
        for (id, c) in &mut game.clients {
            if c.closed {
                closed.push(*id);
                continue;
            }
            let Some(peer) = peers.get(id) else {
                continue;
            };
            for packet in std::mem::take(&mut c.out) {
                match packet.encode() {
                    Ok(frame) => {
                        if peer.sender.try_send(frame).is_err() {
                            closed.push(*id);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("client {id}, encoding response: {e}");
                        closed.push(*id);
                        break;
                    }
                }
            }
        }
        for id in closed {
            if let Some(p) = peers.remove(&id) {
                let _ = p.socket.shutdown(Shutdown::Both);
            }
            game.disconnect(id);
        }
    }
    Ok(())
}
fn find_account(s: &State, name: &str) -> Result {
    let name = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let row = s.one(
        "SELECT * FROM accounts WHERE \"usernameFold\"=$1",
        &[name.into()],
    )?;
    if row.is_none() {
        return Err("no such account".into());
    }
    Ok(row)
}
fn admin(args: &Args, s: &mut State) -> Result<bool> {
    let mut did = false;
    if let Some(a) = &args.add_account {
        if find_account(s, &a[0]).is_ok() {
            return Err("account already exists".into());
        }
        let account = s.authenticate(&a[0], &a[1], true)?;
        println!(
            "Created account {}: {}",
            account["accountID"].int(),
            account["username"].text()
        );
        did = true;
    }
    if let Some(a) = &args.set_password {
        let account = find_account(s, &a[0])?;
        let mut salt = [0; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        s.exec(
            "UPDATE accounts SET \"passwordHash\"=$1,salt=$2 WHERE \"accountID\"=$3",
            &[
                hex::encode(hash_password(&a[1], &salt)).into(),
                hex::encode(salt).into(),
                account["accountID"].clone(),
            ],
        )?;
        println!("Password changed.");
        did = true;
    }
    for (name, banned) in [(&args.ban, 1), (&args.unban, 0)] {
        if let Some(name) = name {
            let account = find_account(s, name)?;
            s.exec(
                "UPDATE accounts SET banned=$1 WHERE \"accountID\"=$2",
                &[banned.into(), account["accountID"].clone()],
            )?;
            println!("Updated account status.");
            did = true;
        }
    }
    if let Some(a) = &args.assign {
        let character = a[0]
            .parse::<i64>()
            .map_err(|_| Error::from("invalid character ID"))?;
        let account = find_account(s, &a[1])?;
        if s.exec(
            "UPDATE characters SET \"accountID\"=$1 WHERE \"characterID\"=$2",
            &[account["accountID"].clone(), character.into()],
        )? == 0
        {
            return Err("no such character".into());
        }
        println!("Character assigned.");
        did = true;
    }
    if let Some(name) = &args.delete_account {
        let account = find_account(s, name)?;
        let id = account["accountID"].clone();
        for c in s.query(
            "SELECT \"characterID\" FROM characters WHERE \"accountID\"=$1",
            std::slice::from_ref(&id),
        )? {
            s.delete_pilot(id.clone(), c["characterID"].clone())?;
        }
        s.exec("DELETE FROM accounts WHERE \"accountID\"=$1", &[id])?;
        println!("Account and its characters deleted.");
        did = true;
    }
    if args.accounts {
        for a in s.query(
            "SELECT \"accountID\",username,banned FROM accounts ORDER BY \"accountID\"",
            &[],
        )? {
            let names = s
                .query(
                    "SELECT \"characterName\" FROM characters WHERE \"accountID\"=$1 ORDER BY \"characterID\"",
                    &[a["accountID"].clone()],
                )?
                .iter()
                .map(|c| c["characterName"].text())
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "{}  {}  {}{}",
                a["accountID"].int(),
                a["username"].text(),
                names,
                if a["banned"].truth() { " [BANNED]" } else { "" }
            );
        }
        did = true;
    }
    if args.summary {
        for table in [
            "accounts",
            "characters",
            "inventory",
            "market_orders",
            "market_transactions",
        ] {
            println!(
                "{table}: {}",
                s.one(&format!("SELECT COUNT(*) AS n FROM {table}"), &[])?["n"].int()
            );
        }
        did = true;
    }
    Ok(did)
}
fn run() -> std::result::Result<(), String> {
    let args = Args::parse();
    if args.health_check {
        let mut socket =
            TcpStream::connect_timeout(&([127, 0, 0, 1], args.port).into(), Duration::from_secs(2))
                .map_err(|e| e.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        let frame = read_frame(&mut socket, MAX_FRAME).map_err(|e| e.to_string())?;
        if pickle::loads(&frame)? != macho::challenge() {
            return Err("invalid game-server handshake".into());
        }
        return Ok(());
    }
    let url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .ok_or("set DATABASE_URL or --database-url to the single PostgreSQL database")?;
    let mut state = if args.check_db {
        State::open_readonly(&url)
    } else {
        State::open(&url)
    }
    .map_err(|e| format!("{e:?}"))?;
    if args.check_db {
        state.check_database().map_err(|e| format!("{e:?}"))?;
        println!(
            "PostgreSQL database passed schema, constraint, catalog and character-location checks."
        );
        return Ok(());
    }
    if args.setup_db {
        state.lock_server().map_err(|e| format!("{e:?}"))?;
        state.boot_market().map_err(|e| format!("{e:?}"))?;
        state.check_database().map_err(|e| format!("{e:?}"))?;
        println!("PostgreSQL setup complete: one database for universe and player state.");
        for table in [
            "solar_systems",
            "system_contents",
            "game_types",
            "accounts",
            "characters",
            "inventory",
            "market_orders",
        ] {
            println!(
                "{table}: {}",
                state
                    .one(&format!("SELECT COUNT(*) AS n FROM {table}"), &[])
                    .map_err(|e| format!("{e:?}"))?["n"]
                    .int()
            );
        }
        return Ok(());
    }
    if admin(&args, &mut state).map_err(|e| format!("{e:?}"))? {
        return Ok(());
    }
    serve(&args, state)
}
fn main() {
    if let Err(e) = run() {
        eprintln!("poc_server: {e}");
        std::process::exit(1);
    }
}
