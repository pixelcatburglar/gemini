use crate::{d, db::Connection, pickle::Value as V, reference};
use rand::RngCore;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

#[derive(Debug)]
pub enum Error {
    Internal(String),
    User(String, V),
}
impl From<String> for Error {
    fn from(v: String) -> Self {
        Self::Internal(v)
    }
}
impl From<&str> for Error {
    fn from(v: &str) -> Self {
        Self::Internal(v.into())
    }
}
impl From<tokio_postgres::Error> for Error {
    fn from(v: tokio_postgres::Error) -> Self {
        Self::Internal(match v.as_db_error() {
            Some(db) => format!("PostgreSQL {}: {}", db.code().code(), db.message()),
            None => v.to_string(),
        })
    }
}
pub type Result<T = V> = std::result::Result<T, Error>;
pub fn user(message: &str) -> Error {
    Error::User("CustomInfo".into(), d!["info"=>message])
}
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}
pub fn eve_time() -> V {
    ((now() * 10_000_000.) as i64 + 116444736000000000).into()
}

pub struct State {
    pub db: Connection,
    pub data: V,
}
impl State {
    pub fn open(url: &str) -> Result<Self> {
        Self::from_connection(Connection::open(url)?)
    }
    pub fn from_connection(db: Connection) -> Result<Self> {
        if db.one(
            "SELECT value FROM meta WHERE key='postgres_schema_version'",
            &[],
        )?["value"]
            .text()
            != "1"
        {
            return Err(
                "unsupported PostgreSQL schema; initialize using the database container".into(),
            );
        }
        let data = reference::load(&db)?;
        Ok(Self { db, data })
    }
    pub fn lock_server(&self) -> Result<()> {
        if !self.one(
            "SELECT pg_try_advisory_lock(hashtext(current_schema()), 665) AS acquired",
            &[],
        )?["acquired"]
            .truth()
        {
            return Err("another game server already owns this database".into());
        }
        Ok(())
    }
    pub fn check_database(&self) -> Result<()> {
        reference::check(&self.db)?;
        if self.one("SELECT value FROM meta WHERE key='schema_version'", &[])?["value"].text()
            != "4"
        {
            return Err("state database needs schema migration".into());
        }
        let known: std::collections::BTreeSet<_> =
            self.c("TYPES").seq().iter().map(|r| r[0].int()).collect();
        for table in [
            "inventory",
            "skills",
            "market_orders",
            "market_transactions",
        ] {
            for r in self.query(&format!("SELECT DISTINCT \"typeID\" FROM {table}"), &[])? {
                if !known.contains(&r["typeID"].int()) {
                    return Err(
                        format!("{table} refers to unknown type {}", r["typeID"].text()).into(),
                    );
                }
            }
        }
        for r in self.query(
            "SELECT DISTINCT \"solarSystemID\",\"constellationID\",\"regionID\" FROM character_location",
            &[],
        )? {
            let count = self.one("SELECT COUNT(*) AS n FROM solar_systems WHERE \"solarSystemID\"=$1 AND \"constellationID\"=$2 AND \"regionID\"=$3", &[r["solarSystemID"].clone(),r["constellationID"].clone(),r["regionID"].clone()])?["n"].int();
            if count != 1 {
                return Err("character location disagrees with universe database".into());
            }
        }
        Ok(())
    }
    pub fn c(&self, name: &str) -> V {
        self.data[&format!("chardata.{name}")[..]].clone()
    }
    pub fn p(&self, name: &str) -> V {
        self.data["chardata.PLACEHOLDERS"]
            .key(&name.into())
            .or(self.data["template.constants"]
                .seq()
                .iter()
                .find(|v| v[0].text() == name)
                .map(|v| v[1].clone())
                .unwrap_or(V::None))
    }
    pub fn template(&self, name: &str) -> V {
        self.data[&format!("template.{name}")[..]].clone()
    }
    pub fn query(&self, sql: &str, args: &[V]) -> Result<Vec<V>> {
        self.db.query(sql, args)
    }
    pub fn one(&self, sql: &str, args: &[V]) -> Result {
        self.db.one(sql, args)
    }
    pub fn exec(&self, sql: &str, args: &[V]) -> Result<usize> {
        self.db.exec(sql, args)
    }
    pub fn insert(&self, table: &str, row: &V, ignore: bool) -> Result<()> {
        // Table and columns are internal schema identifiers, never RPC text.
        let keys = row
            .pairs()
            .iter()
            .map(|(k, _)| format!("\"{}\"", k.text()))
            .collect::<Vec<_>>();
        let placeholders = (1..=keys.len())
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(",");
        let args = row
            .pairs()
            .iter()
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>();
        self.exec(
            &format!(
                "INSERT INTO {table} ({}) VALUES({placeholders}) {}",
                keys.join(","),
                if ignore { "ON CONFLICT DO NOTHING" } else { "" }
            ),
            &args,
        )?;
        Ok(())
    }
    pub fn transaction<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.db
            .batch("BEGIN; SELECT pg_advisory_xact_lock(hashtext(current_schema()),666)")?;
        match f(self) {
            Ok(v) => {
                self.db.batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.db.batch("ROLLBACK");
                Err(e)
            }
        }
    }
    pub fn next_id(&self, table: &str, column: &str, base: i64) -> Result<i64> {
        Ok(self.one(
            &format!(
                "SELECT MAX({}) AS n FROM {}",
                reference::identifier(column)?,
                reference::identifier(table)?
            ),
            &[],
        )?["n"]
            .int()
            .saturating_add(1)
            .max(base))
    }
    pub fn character(&self, id: V) -> Result {
        self.one("SELECT * FROM characters WHERE \"characterID\"=$1", &[id])
    }
    pub fn owns(&self, account: V, id: V) -> Result<bool> {
        Ok(!self
            .one(
                "SELECT 1 AS yes FROM characters WHERE \"accountID\"=$1 AND \"characterID\"=$2",
                &[account, id],
            )?
            .is_none())
    }
    pub fn item(&self, id: V) -> Result {
        self.one("SELECT * FROM inventory WHERE \"itemID\"=$1", &[id])
    }
    pub fn location(&self, id: V) -> Result {
        self.one(
            "SELECT * FROM character_location WHERE \"characterID\"=$1",
            &[id],
        )
    }
    pub fn balance(&self, id: V) -> Result<f64> {
        Ok(self.one("SELECT balance FROM wallet WHERE \"ownerID\"=$1", &[id])?["balance"].float())
    }
    pub fn money(&self, id: V, delta: f64) -> Result<()> {
        if !delta.is_finite() {
            return Err("non-finite wallet adjustment".into());
        }
        self.exec("INSERT INTO wallet(\"ownerID\",balance) VALUES($1,$2) ON CONFLICT(\"ownerID\") DO UPDATE SET balance=wallet.balance+excluded.balance",&[id,delta.into()])?;
        Ok(())
    }
    pub fn quantity(&self, id: V, n: i64) -> Result<()> {
        if n <= 0 {
            self.exec("DELETE FROM inventory WHERE \"itemID\"=$1", &[id])?;
        } else {
            self.exec(
                "UPDATE inventory SET quantity=$1 WHERE \"itemID\"=$2",
                &[n.into(), id],
            )?;
        }
        Ok(())
    }
    pub fn authenticate(&mut self, username: &str, password: &str, open: bool) -> Result {
        let username = username.split_whitespace().collect::<Vec<_>>().join(" ");
        let fold = username.to_lowercase();
        let denied = || Error::User("LoginAuthFailed".into(), V::None);
        if fold.is_empty() {
            return Err(denied());
        }
        let mut account = self.one(
            "SELECT * FROM accounts WHERE \"usernameFold\"=$1",
            &[fold.clone().into()],
        )?;
        if account.is_none() {
            if !open {
                return Err(denied());
            }
            let mut salt = [0; 16];
            rand::thread_rng().fill_bytes(&mut salt);
            let hash = hash_password(password, &salt);
            account=self.transaction(|s|{
                let first=s.one("SELECT COUNT(*) AS n FROM accounts",&[])?["n"].int()==0;
                let created=s.one("INSERT INTO accounts(username,\"usernameFold\",\"passwordHash\",salt,created) VALUES($1,$2,$3,$4,$5) RETURNING \"accountID\"",&[
                    username.clone().into(),fold.clone().into(),hex::encode(hash).into(),hex::encode(salt).into(),now().into()])?;
                let id=created["accountID"].clone();
                if first { s.exec("UPDATE characters SET \"accountID\"=$1 WHERE \"accountID\" IS NULL",std::slice::from_ref(&id))?; }
                s.one("SELECT * FROM accounts WHERE \"accountID\"=$1",&[id])
            })?;
        } else {
            let salt = hex::decode(account["salt"].text()).map_err(|_| "invalid account salt")?;
            let want =
                hex::decode(account["passwordHash"].text()).map_err(|_| "invalid account hash")?;
            if !bool::from(hash_password(password, &salt).as_slice().ct_eq(&want)) {
                return Err(denied());
            }
        }
        if account["banned"].truth() {
            return Err(denied());
        }
        self.exec(
            "UPDATE accounts SET \"lastLogin\"=$1 WHERE \"accountID\"=$2",
            &[now().into(), account["accountID"].clone()],
        )?;
        Ok(account)
    }
}
pub fn hash_password(password: &str, salt: &[u8]) -> [u8; 32] {
    pbkdf2::pbkdf2_hmac_array::<pbkdf2::sha2::Sha256, 32>(password.as_bytes(), salt, 100_000)
}
