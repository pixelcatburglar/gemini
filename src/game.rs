use crate::{
    d,
    destiny::Rat,
    l,
    macho::{self, Packet},
    market::{self, Changes, Order},
    pickle::Value as V,
    state::{Error, Result, State, eve_time, user},
    t,
    value::{rows, rowset},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

pub struct Binding {
    pub kind: String,
    pub context: V,
    pub shell: V,
    pub epoch: Instant,
    pub state_sent: bool,
}
pub struct Client {
    pub id: i64,
    pub account: V,
    pub session: V,
    pub objects: BTreeMap<String, Binding>,
    pub next_object: i64,
    pub order_shells: BTreeMap<(i64, i64), V>,
    pub skill_shell: V,
    pub rooms: BTreeSet<(i64, i64)>,
    pub targets: BTreeSet<i64>,
    pub ballpark: Option<String>,
    pub flight: Option<crate::space::Flight>,
    pub out: Vec<Packet>,
    pub deferred: Vec<Packet>,
    pub closed: bool,
}
impl Client {
    pub fn new(id: i64) -> Self {
        Self {
            id,
            account: V::None,
            session: d![],
            objects: BTreeMap::new(),
            next_object: 1,
            order_shells: BTreeMap::new(),
            skill_shell: V::None,
            rooms: BTreeSet::new(),
            targets: BTreeSet::new(),
            ballpark: None,
            flight: None,
            out: vec![],
            deferred: vec![],
            closed: false,
        }
    }
    pub fn char_id(&self) -> V {
        self.session["charid"].clone()
    }
    pub fn ship(&self) -> V {
        self.session["shipid"].clone()
    }
    pub fn bind(&mut self, kind: &str, context: V) -> V {
        let id = format!("N=1:{}", self.next_object);
        self.next_object += 1;
        self.objects.insert(
            id.clone(),
            Binding {
                kind: kind.into(),
                context,
                shell: V::None,
                epoch: Instant::now(),
                state_sent: false,
            },
        );
        V::Persistent(Box::new(id.into()))
    }
    pub fn emit(&mut self, packet: Packet, defer: bool) {
        if defer {
            self.deferred.push(packet)
        } else {
            self.out.push(packet)
        }
    }
    pub fn broadcast(&mut self, event: &str, args: V, defer: bool) -> Result<()> {
        self.emit(
            Packet {
                command: 12,
                source: macho::node(),
                destination: t!["B", event],
                user_id: self.account.clone(),
                channel: V::None,
                body: vec![macho::broadcast_payload(args)?],
            },
            defer,
        );
        Ok(())
    }
    pub fn callback(&mut self, shell: V, method: &str, args: V, defer: bool) -> Result<()> {
        let object = match shell {
            V::Persistent(pid) => *pid,
            V::Tuple(items) if items.len() == 2 => items[1].clone(),
            v => v,
        };
        if !matches!(object, V::Bytes(_) | V::Unicode(_)) {
            return Ok(());
        }
        self.emit(
            Packet {
                command: 12,
                source: macho::node(),
                destination: macho::client(self.id),
                user_id: self.account.clone(),
                channel: V::None,
                body: vec![macho::callback_payload(object, method, args)?],
            },
            defer,
        );
        Ok(())
    }
    pub fn change_session(&mut self, attrs: V) -> Result<()> {
        let mut changes = d![];
        for (key, value) in attrs.pairs() {
            changes.set(key.clone(), t![self.session.key(key), value.clone()]);
            self.session.set(key.clone(), value.clone());
        }
        self.emit(
            Packet {
                command: 16,
                source: macho::node(),
                destination: macho::client(self.id),
                user_id: self.account.clone(),
                channel: "sessionchange".into(),
                body: vec![t![0, changes]],
            },
            false,
        );
        Ok(())
    }
    pub fn viewer(&self) -> V {
        d!["stationID"=>self.session["stationid"].clone(),"solarSystemID"=>self.session["solarsystemid"].clone(),"constellationID"=>self.session["constellationid"].clone(),"regionID"=>self.session["regionid"].clone()]
    }
}

pub struct Game {
    pub state: State,
    pub clients: BTreeMap<i64, Client>,
    pub channels: BTreeMap<String, i64>,
    pub open: bool,
    pub error_id: i64,
    pub rat: Rat,
}
impl Game {
    pub fn new(state: State, open: bool) -> Self {
        Self {
            state,
            clients: BTreeMap::new(),
            channels: BTreeMap::new(),
            open,
            error_id: 0,
            rat: Rat::default(),
        }
    }
    pub fn handle(&mut self, id: i64, packet: Packet) {
        let Some(mut conn) = self.clients.remove(&id) else {
            return;
        };
        let result = self.handle_inner(&mut conn, &packet);
        if let Err(error) = result {
            self.error_id += 1;
            eprintln!("client {id}, command {}: {error:?}", packet.command);
            conn.out.push(Packet {
                command: 15,
                source: packet.destination.clone(),
                destination: packet.source.clone(),
                user_id: if packet.command == 0 {
                    V::None
                } else {
                    conn.account.clone()
                },
                channel: packet.channel.clone(),
                body: vec![
                    packet.command.into(),
                    2.into(),
                    macho::error_payload(&error, self.error_id),
                ],
            });
            conn.deferred.clear();
        }
        self.clients.insert(id, conn);
    }
    fn handle_inner(&mut self, c: &mut Client, p: &Packet) -> Result<()> {
        match p.command {
            0 => {
                if p.body.len() < 3 {
                    return Err("authentication needs client info, username and password".into());
                }
                let account =
                    self.state
                        .authenticate(&p.body[1].text(), &p.body[2].text(), self.open)?;
                for other in self.clients.values_mut() {
                    if other.account == account["accountID"] {
                        other.closed = true;
                    }
                }
                c.account = account["accountID"].clone();
                let role = 3 | account["role"].int();
                c.out.push(Packet {
                    command: 1,
                    source: p.destination.clone(),
                    destination: p.source.clone(),
                    user_id: c.account.clone(),
                    channel: p.channel.clone(),
                    body: vec![
                        d!["version.number"=>10.0,"version.build"=>665,"version.project"=>"Gemini"],
                        c.account.clone(),
                        role.into(),
                        macho::object_payload(&self.state.template("auth"))?,
                    ],
                });
                c.session = d!["userid"=>c.account.clone(),"role"=>role,"languageID"=>"EN"];
                for key in [
                    "charid",
                    "corpid",
                    "shipid",
                    "stationid",
                    "solarsystemid",
                    "locationid",
                    "regionid",
                    "constellationid",
                    "gangid",
                ] {
                    c.session.set(key, V::None);
                }
                c.out.push(Packet {
                    command: 18,
                    source: macho::node(),
                    destination: macho::client(c.id),
                    user_id: c.account.clone(),
                    channel: "sessionchange".into(),
                    body: vec![c.session.clone()],
                });
            }
            6 => {
                let call = macho::decode_call(p.body.first().unwrap_or(&V::None))?;
                let value = if call.object.is_none() {
                    self.dispatch(
                        c,
                        &macho::address_service(&p.destination).text(),
                        &call.method,
                        call.args,
                        call.keywords,
                    )?
                } else {
                    self.dispatch_object(
                        c,
                        &call.object.text(),
                        &call.method,
                        call.args,
                        call.keywords,
                    )?
                };
                c.out.push(Packet {
                    command: 7,
                    source: p.destination.clone(),
                    destination: p.source.clone(),
                    user_id: c.account.clone(),
                    channel: V::None,
                    body: vec![macho::object_payload(&value)?],
                });
                c.out.append(&mut c.deferred);
            }
            10 => {
                let service = macho::address_service(&p.destination);
                let src = if service.is_none() {
                    macho::node()
                } else {
                    t!["N", 1, service]
                };
                c.out.push(Packet {
                    command: 11,
                    source: src,
                    destination: p.source.clone(),
                    user_id: c.account.clone(),
                    channel: V::None,
                    body: vec![1.into(), V::None],
                });
            }
            12 => {
                if let Some(call) = macho::notification_call(p.body.first().unwrap_or(&V::None))?
                    && !call.object.is_none()
                    && call.method == "ClientHasReleasedThisObject"
                {
                    let oid = call.object.text();
                    if let Some(b) = c.objects.remove(&oid)
                        && b.kind == "chat"
                    {
                        self.leave_room(c, (b.context[0].int(), b.context[1].int()))?;
                    }
                }
            }
            _ => eprintln!("client {}: ignoring command {}", c.id, p.command),
        }
        Ok(())
    }
    pub fn dispatch(
        &mut self,
        c: &mut Client,
        service: &str,
        method: &str,
        args: V,
        keywords: V,
    ) -> Result {
        let args = self
            .state
            .arguments("service", service, method, args, keywords)?;
        if method == "Ping" {
            return Ok(eve_time());
        }
        let s = &mut self.state;
        match (service, method) {
            ("machoNet", "GetTime") => Ok(0.into()),
            ("machoNet", "GetCachableObject") => {
                let value = s.data["bulkdata.OBJECTS"].key(&args[1]);
                if value.is_none() {
                    return Err("unknown cached object".into());
                }
                Ok(t![
                    args[0].clone(),
                    s.data["bulkdata.VERSION"].clone(),
                    1,
                    value
                ])
            }
            ("config", "GetInitVals") => Ok(d![]),
            ("config", "GetUnits") => Ok(s.template("units")),
            ("config", "GetMapHierarchy") => Ok(s.template("map_hierarchy")),
            ("config", "GetLocationsEx") => s.locations_ex(&flatten(&args)),
            ("config", "GetOwnersEx") => s.owners_ex(&flatten(&args)),
            ("config", "GetCelestialStatistic") => Ok(V::List(
                s.template("contents")
                    .seq()
                    .iter()
                    .filter(|b| b["itemID"] == args[0])
                    .map(|b| {
                        s.data_row(
                            "CELESTIAL_STATISTIC_HEADER",
                            vec![
                                format!("{:.0} m", b["radius"].float()).into(),
                                format!("{:.0} m", b["orbitRadius"].float()).into(),
                            ],
                        )
                    })
                    .collect(),
            )),
            ("config", "GetMapObjects") => Ok(if args[0] != s.c("SOLAR_SYSTEM_ID") {
                l![]
            } else {
                V::List(
                    s.template("contents")
                        .seq()
                        .iter()
                        .map(|b| {
                            s.data_row(
                                "MAP_OBJECT_HEADER",
                                ["itemID", "typeID", "itemName", "orbitID"]
                                    .iter()
                                    .map(|k| b[*k].clone())
                                    .collect(),
                            )
                        })
                        .collect(),
                )
            }),
            ("character", "GetCharacterInfo") => s.character_rows(c.account.clone()),
            ("character", "GetCharCreationInfo") => Ok(s.template("char_creation_info")),
            ("character", "CreateCharacter2") => s.create_pilot(c.account.clone(), &args),
            ("character", "SelectCharacterID") => {
                if !s.owns(c.account.clone(), args[0].clone())? {
                    return Err(Error::User("CharacterNotFound".into(), d![]));
                }
                let mut attrs = s.session_for(args[0].clone())?;
                if !attrs["corpid"].is_none() && !c.session["corpid"].truth() {
                    c.change_session(d!["corpid"=>attrs["corpid"].clone()])?;
                    if let V::Dict(p) = &mut attrs {
                        p.retain(|(k, _)| k.text() != "corpid");
                    }
                }
                c.change_session(attrs)?;
                Ok(V::None)
            }
            ("character", "DeleteCharacter") => {
                if args[0] == c.char_id() || !s.delete_pilot(c.account.clone(), args[0].clone())? {
                    Ok(t!["CharacterDeleteFailed", d![]])
                } else {
                    Ok(V::None)
                }
            }
            ("character", "GetCharacterAppearance") => s.appearance(args[0].clone()),
            ("character", "GetOtherCharacterInfo") => s.other_info(args[0].clone()),
            ("character", "InsertCharacterAppearance") => {
                if !s.owns(c.account.clone(), args[0].clone())? {
                    return Err(Error::User("CharacterNotFound".into(), d![]));
                }
                s.transaction(|s| {
                    s.exec(
                        "DELETE FROM character_appearance WHERE \"characterID\"=$1",
                        &[args[0].clone()],
                    )?;
                    for (k, v) in args[1].pairs() {
                        s.exec(
                            "INSERT INTO character_appearance VALUES($1,$2,$3)",
                            &[args[0].clone(), k.text().into(), v.text().into()],
                        )?;
                    }
                    Ok(V::None)
                })
            }
            ("charmgr", "GetPublicInfo") => s.public_info(args[0].or(c.char_id())),
            ("shipSvc", "GetShipAppearance") => Ok(l![]),
            ("station", "GetOffices") => Ok(s.template("offices")),
            ("repair", "GetDamageReport") => s.repair_report(c, &args[0]),
            ("reprocessing", "GetOptionsForItemTypes") => {
                s.require_station(c)?;
                s.reprocessing_options(&args[0])
            }
            ("repair", "RepairShip" | "RepairModule" | "DamageModule") => {
                let result = s.repair(
                    c,
                    &args[0],
                    &args[1],
                    method == "RepairShip",
                    method == "DamageModule",
                )?;
                self.notify_wallet(c, true)?;
                Ok(result)
            }
            ("reprocessing", "GetQuote") => s.reprocessing_quote(c, &args[0]),
            ("reprocessing", "Reprocess") => {
                let changes = s.reprocess(c, &args[0])?;
                self.notify_inventory(
                    c,
                    &changes.added,
                    &changes.changed,
                    &changes.removed,
                    true,
                    true,
                )?;
                Ok(V::None)
            }
            ("pager", "GetPages") => Ok(l![]),
            ("pager", "FindOwner") => {
                let needle = args[0].text().to_lowercase();
                if needle.is_empty() {
                    return Ok(l![]);
                }
                let ids = s
                    .query("SELECT \"characterID\" FROM characters", &[])?
                    .into_iter()
                    .map(|c| c["characterID"].int())
                    .collect::<BTreeSet<_>>();
                Ok(rows(
                    s.c("OWNER_HEADER"),
                    s.owners()?
                        .into_iter()
                        .filter(|o| {
                            o[1].text().to_lowercase().starts_with(&needle)
                                && (args[2] != s.c("GROUP_CHARACTER") || ids.contains(&o[0].int()))
                        })
                        .map(|o| o.seq().to_vec())
                        .collect(),
                ))
            }
            ("gangsta", "GetGangInfoForChar") => Ok(t![V::None, V::None, V::None, V::None]),
            (
                "billMgr",
                "CharGetBills"
                | "CharGetBillsReceivable"
                | "GetCorporationBills"
                | "GetCorporationBillsReceivable",
            ) => Ok(s.template("bills")),
            ("missionMgr", "GetAvailableMissions" | "GetMyMissions" | "GetIssuedMissions") => {
                Ok(l![])
            }
            ("corpmgr", _) => self.corp(c, method, &args),
            ("market", _) => self.market(c, method, &args),
            ("marketBroker", "GetMarketRegion") => Ok(c.session["regionid"].or(s.c("REGION_ID"))),
            ("inventorymgr", "GetInventory") => Ok(c.bind(
                "inventory",
                d!["container"=>args[0].clone(),"owner_id"=>args[1].clone()],
            )),
            ("inventorymgr", "GetInventoryFromId") => {
                Ok(c.bind("inventory", d!["item_id"=>args[0].clone()]))
            }
            ("inventorymgr", "MachoBindObject") => Ok(c.bind("inventory", d![])),
            ("inventorymgr" | "dogmaIM" | "beyonce" | "chatx", "MachoResolveObject") => {
                Ok(1.into())
            }
            ("skillMgr", "SetShell") => {
                c.skill_shell = args[0].clone();
                Ok(V::None)
            }
            ("skillMgr", "CharGetSkills") => Ok(V::List(
                s.query(
                    "SELECT \"typeID\" FROM skills WHERE \"ownerID\"=$1 ORDER BY \"typeID\"",
                    &[c.char_id()],
                )?
                .iter()
                .map(|r| r["typeID"].clone())
                .collect(),
            )),
            ("skillMgr", "GetSkillsInTraining") | ("dogmaIM", "GetSkillPrerequisites") => Ok(l![]),
            ("dogmaIM", "GetAttributeTypes") => Ok(s.template("attribute_types")),
            ("dogmaIM", "MachoBindObject") => Ok(c.bind("dogma", d![])),
            ("ship", "GetShipAccess") => Ok(c.bind("ship", d![])),
            ("entity", "GetEntityAccess") => Ok(c.bind("entity", d![])),
            ("beyonce", "MachoBindObject") => {
                self.state.require_space(c)?;
                if c.flight.is_none() {
                    c.flight = Some(self.state.load_flight(c.ship())?);
                }
                if let Some(id) = &c.ballpark
                    && c.objects.contains_key(id)
                {
                    return Ok(V::Persistent(Box::new(id.clone().into())));
                }
                let reference = c.bind("park", d!["ship_id"=>c.ship()]);
                let V::Persistent(id) = &reference else {
                    unreachable!()
                };
                let id = id.text();
                c.targets.clear();
                c.ballpark = Some(id.clone());
                self.push_state(c, &id, true)?;
                let (ball, slim) = crate::destiny::player_objects(&self.state, c)?;
                for other in self.clients.values_mut().filter(|o| {
                    o.session["solarsystemid"] == c.session["solarsystemid"]
                        && o.session["stationid"].is_none()
                }) {
                    if let Some(b) = other.ballpark.as_ref().and_then(|id| other.objects.get(id)) {
                        other.broadcast(
                            "OnDestinyUpdate",
                            t![crate::destiny::action(
                                b.epoch,
                                "AddBalls",
                                vec![t![l![t![ball.clone(), slim.clone()]], d![]]]
                            )],
                            false,
                        )?;
                    }
                }
                Ok(reference)
            }
            ("chatx", _) => self.chat_service(c, method, &args),
            _ => Err(format!("{service}.{method} is not implemented by this POC server").into()),
        }
    }
    fn corp(&mut self, c: &Client, method: &str, args: &V) -> Result {
        let s = &self.state;
        Ok(match method {
            "GetPublicInfo" => s.corporation()?,
            "GetCorporations" => {
                let id = if matches!(args[0], V::List(_) | V::Tuple(_)) {
                    args[0][0].clone()
                } else {
                    args[0].clone()
                };
                if id.is_none() || id.int() == -1 {
                    l![s.corporation()?]
                } else {
                    s.corporation()?
                }
            }
            "GetCorporationsAtStation" => {
                if c.session["stationid"] == s.c("STATION_ID") {
                    l![s.corporation()?]
                } else {
                    l![]
                }
            }
            "GetCorporationIDForCharacter" => s.pilot(args[0].clone())?["corporationID"].clone(),
            "GetMembersInfo" | "GetMembersWithNamesWithMoreThan5" => {
                s.members(c.session["corpid"].clone(), V::None)?
            }
            "GetMembers" => s.members(args[0].clone(), args[1].clone())?,
            "GetRoles" => s.template("roles"),
            "GetApplications"
            | "GetVoteCasesByCorporation"
            | "GetVoteCaseOptions"
            | "GetVotes"
            | "GetSanctionedActionsByCorporation"
            | "GetWars"
            | "GetCorporationStandings" => l![],
            "UserIsCEO" | "UserIsActiveCEO" | "UserIsActiveCEOorEquivalent" => {
                i32::from(c.char_id() == s.c("CORPORATION_ID")).into()
            }
            "CanViewVotes" => 1.into(),
            "GetCostForCreatingACorporation" => 1600000.into(),
            "DoesCharactersCorpOwnThisStation" => i32::from(
                !args[1].is_none()
                    && args[1] == c.session["stationid"]
                    && c.session["corpid"] == s.c("CORPORATION_ID"),
            )
            .into(),
            "AreAnyOfficesAvailableForRenting"
            | "AreAnyFactorySlotsAvailableForRenting"
            | "GetGrantableRolesForCharacter"
            | "CanBeKickedOut"
            | "CharCanRunForCEO"
            | "UserCanCreateCorporation"
            | "UserIsAccountantOrEquivalent"
            | "UserIsPersonnelManagerOrEquivalent"
            | "UserIsStationManager"
            | "UserIsFactoryManager" => 0.into(),
            _ => {
                return Err(
                    format!("corpmgr.{method} is not implemented by this POC server").into(),
                );
            }
        })
    }
    fn market(&mut self, c: &mut Client, method: &str, a: &V) -> Result {
        let s = &mut self.state;
        let viewer = c.viewer();
        match method {
            "GetMarketGroups" => Ok(s.template("market_groups")),
            "GetGroupOrders" | "GetGroupQuote" => {
                let mut out = d![];
                for typ in s.types_under(a[0].clone()) {
                    let value = if method == "GetGroupOrders" {
                        s.book(typ.clone(), 0.into(), &viewer, &a[1])?
                    } else {
                        s.quote(typ.clone(), 0.into(), &viewer, &a[1])?
                    };
                    out.set(typ, value);
                }
                Ok(out)
            }
            "GetPriceHistory" => s.history(market::type_info(&a[1])?.0),
            "GetOrders" | "GetQuote" | "RemoveOrderShell" => {
                let (typ, sub) = market::type_info(&a[0])?;
                if method == "GetOrders" {
                    if !a[1].is_none() {
                        c.order_shells.insert((typ.int(), sub.int()), a[1].clone());
                    }
                    s.book(typ, sub, &viewer, &s.p("rangeRegion"))
                } else if method == "GetQuote" {
                    s.quote(typ, sub, &viewer, &a[1])
                } else {
                    c.order_shells.remove(&(typ.int(), sub.int()));
                    Ok(V::None)
                }
            }
            "CharGetOrders" => s.my_orders(c.char_id(), &viewer, &a[0]),
            "CharCancelOrder" => {
                let result = s.cancel_order(c.char_id(), a[0].clone())?;
                self.notify_market(c, result, true)?;
                Ok(V::None)
            }
            "CharPlaceOrder" | "CharPlaceRemoteBid" => {
                let remote = method == "CharPlaceRemoteBid";
                let offset = usize::from(remote);
                let (typ, sub) = market::type_info(&a[offset])?;
                let order = Order {
                    owner: c.char_id(),
                    type_id: typ,
                    sub,
                    quantity: a[offset + 1].int(),
                    price: a[offset + 2].int(),
                    range: if remote {
                        s.p("rangeRegion")
                    } else {
                        a[3].clone()
                    },
                    duration: if remote { 0.into() } else { a[4].clone() },
                    bid: if remote {
                        a[4].or(1.into()).truth()
                    } else {
                        a[5].truth()
                    },
                    item: if remote { V::None } else { a[6].clone() },
                    station: if remote { a[0].clone() } else { V::None },
                };
                let result = s.place_order(order)?;
                let changed = result.filled > 0 || result.order.is_some();
                self.notify_market(c, result, changed)?;
                Ok(V::None)
            }
            _ => Err(format!("market.{method} is not implemented by this POC server").into()),
        }
    }
    pub fn notify_inventory(
        &self,
        c: &mut Client,
        added: &[V],
        changed: &[V],
        removed: &[V],
        all: bool,
        defer: bool,
    ) -> Result<()> {
        let shells = c
            .objects
            .values()
            .filter(|b| {
                b.kind == "inventory"
                    && (b.context["container"] == self.state.c("CONTAINER_HANGAR")
                        || (all && !b.context["item_id"].is_none()))
            })
            .map(|b| b.shell.clone())
            .collect::<Vec<_>>();
        for (action, ids) in [("r", removed), ("a", added), ("m", changed)] {
            for id in ids {
                let item = self.state.item(id.clone())?;
                let gone = action == "r" || item.is_none();
                let rec = if gone {
                    self.state.gone_item(id.clone())
                } else {
                    self.state.inventory_row(&item)
                };
                for shell in &shells {
                    c.callback(
                        shell.clone(),
                        "OnModify",
                        t![if gone { "r" } else { action }, rec.clone(), V::None],
                        defer,
                    )?;
                }
            }
        }
        Ok(())
    }
    fn notify_wallet(&self, c: &mut Client, defer: bool) -> Result<()> {
        let shells = c
            .objects
            .values()
            .filter(|b| {
                b.kind == "inventory" && b.context["container"] == self.state.c("CONTAINER_WALLET")
            })
            .map(|b| b.shell.clone())
            .collect::<Vec<_>>();
        for shell in shells {
            c.callback(shell, "OnModify", t![], defer)?;
        }
        Ok(())
    }
    fn notify_market(&mut self, c: &mut Client, result: Changes, orders: bool) -> Result<()> {
        if result.spent != 0. || result.earned != 0. || orders {
            self.notify_wallet(c, true)?;
        }
        self.notify_inventory(
            c,
            &result.added,
            &result.changed,
            &result.removed,
            false,
            true,
        )?;
        if orders {
            for ((typ, sub), shell) in c.order_shells.clone() {
                c.callback(
                    shell,
                    "OnOrderChanged",
                    t![market::order_type(typ.into(), sub.into())],
                    true,
                )?;
            }
            c.broadcast("OnOwnOrderChanged", t![], true)?;
        }
        for (id, (added, changed)) in result.others {
            let ids = self
                .clients
                .iter()
                .filter(|(_, c)| c.char_id().int() == id)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            for id in ids {
                let mut other = self.clients.remove(&id).unwrap();
                self.notify_wallet(&mut other, false)?;
                self.notify_inventory(&mut other, &added, &changed, &[], false, false)?;
                other.broadcast("OnOwnOrderChanged", t![], false)?;
                self.clients.insert(id, other);
            }
        }
        Ok(())
    }
    fn chat_service(&mut self, c: &mut Client, method: &str, a: &V) -> Result {
        let s = &self.state;
        match method {
            "GetChannels" => {
                let mut lines = vec![];
                for (id, typ) in [
                    (c.session["locationid"].clone(), 1),
                    (c.session["corpid"].clone(), 2),
                ] {
                    if !id.is_none() {
                        lines.push(vec![id.clone(), typ.into(), id, 1.into(), d![]]);
                    }
                }
                for (name, id) in &self.channels {
                    if c.rooms.contains(&(0, *id)) {
                        lines.push(vec![
                            (*id).into(),
                            0.into(),
                            name.clone().into(),
                            1.into(),
                            d![],
                        ]);
                    }
                }
                Ok(rowset(s.c("CHANNEL_HEADER"), lines))
            }
            "EstablishChannel" => {
                let name = a[0].text();
                if name.is_empty() {
                    return Err(user("A channel needs a name."));
                }
                let next = 200000000 + self.channels.len() as i64;
                let id = *self.channels.entry(name.clone()).or_insert(next);
                Ok(s.data_row(
                    "CHANNEL_HEADER",
                    vec![id.into(), 0.into(), name.into(), 1.into(), d![]],
                ))
            }
            "MachoBindObject" => {
                if a[0].seq().len() != 2 {
                    return Err("chatx binds on (channelID, channelType)".into());
                }
                let room = (a[0][1].int(), a[0][0].int());
                if c.rooms.insert(room) {
                    let info = self.member(c)?;
                    self.tell_room(c, room, "OnChatJoin", t![room.0, room.1, info])?;
                }
                Ok(c.bind("chat", t![room.0, room.1]))
            }
            _ => Err(format!("chatx.{method} is not implemented by this POC server").into()),
        }
    }
    pub fn member(&self, c: &Client) -> Result {
        Ok(self.state.data_row(
            "CHANNEL_MEMBER_HEADER",
            vec![
                c.char_id(),
                c.session["corpid"].clone(),
                0.into(),
                self.state.owner_name(c.char_id())?,
            ],
        ))
    }
    pub fn tell_room(&mut self, c: &Client, room: (i64, i64), event: &str, args: V) -> Result<()> {
        for other in self.clients.values_mut() {
            if other.id != c.id && other.rooms.contains(&room) {
                other.broadcast(event, args.clone(), false)?;
            }
        }
        Ok(())
    }
    pub fn leave_room(&mut self, c: &mut Client, room: (i64, i64)) -> Result<()> {
        if c.rooms.remove(&room) {
            self.tell_room(c, room, "OnChatLeave", t![room.0, room.1, c.char_id()])?;
        }
        Ok(())
    }
    pub fn disconnect(&mut self, id: i64) {
        if let Some(mut c) = self.clients.remove(&id) {
            if let Some(f) = c.flight.as_mut() {
                f.update();
                if let Err(e) = self.state.save_flight(c.session["shipid"].clone(), f) {
                    eprintln!("saving disconnected ship: {e:?}");
                }
                let ship = c.ship();
                let _ = self.space_event(&mut c, "RemoveBall", vec![ship, (-1).into()], false);
            }
            for room in c.rooms.clone() {
                let _ = self.leave_room(&mut c, room);
            }
        }
    }
}
fn flatten(args: &V) -> Vec<V> {
    args.seq()
        .iter()
        .flat_map(|v| match v {
            V::List(v) | V::Tuple(v) => v.clone(),
            V::None => vec![],
            v => vec![v.clone()],
        })
        .collect()
}
