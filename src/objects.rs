use crate::{
    d,
    destiny::{self, RAT_ID, Rat},
    game::{Client, Game},
    l,
    pickle::Value as V,
    state::{Error, Result, eve_time, user},
    t,
    value::rowset,
};
use std::time::Instant;

impl Game {
    pub fn dispatch_object(
        &mut self,
        c: &mut Client,
        id: &str,
        method: &str,
        a: V,
        kw: V,
    ) -> Result {
        let b = c.objects.get(id).ok_or("unknown bound object")?;
        let kind = b.kind.clone();
        let context = b.context.clone();
        let a = self.state.arguments("object", &kind, method, a, kw)?;
        if method == "DisconnectObject" {
            return Ok(V::None);
        }
        match kind.as_str() {
            "inventory" => self.inventory(c, id, method, &context, &a),
            "ship" => {
                match method {
                    "Board" => {
                        if !self.state.hangar_ids(c.char_id())?.contains(&a[0]) {
                            return Err("no such ship in this character's hangar".into());
                        }
                        c.change_session(
                            d!["shipid"=>a[0].clone(),"locrole"=>self.state.c("FLAG_PILOT")],
                        )?;
                    }
                    "Eject" => c.change_session(d!["shipid"=>V::None,"locrole"=>V::None])?,
                    "Undock" => {
                        if c.ship().is_none() {
                            return Err("you have to board a ship before undocking".into());
                        }
                        if c.session["stationid"].is_none() {
                            return Err("you are not docked".into());
                        }
                        c.change_session(d!["stationid"=>V::None,"stationItem"=>V::None,"locationid"=>c.session["solarsystemid"].clone()])?;
                    }
                    "Jettison" => {
                        return Err(
                            "Jettison needs a solar system to jettison into; this server has none."
                                .into(),
                        );
                    }
                    _ => {
                        return Err(
                            format!("ship.{method} is not implemented by this POC server").into(),
                        );
                    }
                }
                Ok(V::None)
            }
            "dogma" => self.dogma(c, method, &a),
            "park" => self.park(c, id, method, &a),
            "chat" => self.chat_object(c, method, &context, &a),
            "entity"
                if matches!(
                    method,
                    "CmdAttack" | "CmdMine" | "CmdMineRepeatedly" | "CmdReturnHome"
                ) =>
            {
                Err(user("This server has no drones to command."))
            }
            _ => Err(format!("{kind}.{method} is not implemented by this POC server").into()),
        }
    }
    fn inventory(&mut self, c: &mut Client, id: &str, method: &str, ctx: &V, a: &V) -> Result {
        let s = &mut self.state;
        let ship = if ctx["item_id"] == c.session["stationid"] {
            V::None
        } else {
            ctx["item_id"].clone()
        };
        match method {
            "SetShell" => {
                c.objects.get_mut(id).unwrap().shell = a[0].clone();
                Ok(V::None)
            }
            "List" | "ListStations" => Ok(if ctx["container"] == s.c("CONTAINER_WALLET") {
                l![]
            } else {
                V::List(
                    s.hangar(c.char_id())?
                        .iter()
                        .map(|i| s.inventory_row(i))
                        .collect(),
                )
            }),
            "GetMoney" => s.wallet_item(ctx["owner_id"].or(c.char_id())),
            "GetVouchers" => Ok(l![]),
            "GetItem" => {
                let item = ctx["item_id"].clone();
                if item.is_none() {
                    return Err("this inventory is a container, not an item".into());
                }
                if s.hangar_ids(c.char_id())?.contains(&item) {
                    return s.ship_item(item, c.session["solarsystemid"].clone());
                }
                if item == c.session["stationid"] {
                    return Ok(s.template("station_item"));
                }
                Err("no inventory for that item".into())
            }
            "ListExtra" => {
                if ship.is_none() {
                    return Ok(l![]);
                }
                let mut items = s.systems(ship.clone())?;
                items.extend(s.cargo(ship)?);
                Ok(V::List(items.iter().map(|i| s.inventory_row(i)).collect()))
            }
            "ListCargo" => {
                if ship.is_none() {
                    return Err("this inventory has no cargo hold".into());
                }
                Ok(V::List(
                    s.cargo(ship)?.iter().map(|i| s.inventory_row(i)).collect(),
                ))
            }
            "GetCapacity" => {
                if ship.is_none() {
                    return Err("this inventory has no capacity".into());
                }
                Ok(s.data_row(
                    "CAPACITY_HEADER",
                    vec![
                        s.type_info(s.ship_type(ship.clone())?)["capacity"].clone(),
                        s.cargo_used(ship)?.into(),
                    ],
                ))
            }
            "Add" => {
                let stored = s.item(a[0].clone())?;
                if stored.is_none() {
                    return Err(user("There is no such item to move."));
                }
                if stored["ownerID"] != c.char_id() {
                    return Err(Error::User("CantDoThatWithSomeoneElsesStuff".into(), d![]));
                }
                if stored["flag"] == s.p("flagMarket") {
                    return Err(user(
                        "That crate is held in escrow by one of your sell orders. Cancel the order to get it back.",
                    ));
                }
                let target = ship.or(c.session["stationid"].clone());
                if target.is_none() {
                    return Err(user(
                        "You are not docked -- there is no hangar to put that in.",
                    ));
                }
                let flag = a[2].or(stored["flag"].clone());
                let qty = a[1].or(stored["quantity"].clone()).int();
                if flag == s.c("FLAG_CARGO")
                    && s.category(stored["typeID"].clone()) == s.c("CATEGORY_SHIP")
                {
                    return Err(user(
                        "A ship will not fit in a cargo hold. Leave it in the hangar and board it instead.",
                    ));
                }
                if qty < 1 || qty > stored["quantity"].int() {
                    return Err(user("You do not have that quantity to move."));
                }
                let same = stored["locationID"] == target && stored["flag"] == flag;
                if same && qty == stored["quantity"].int() {
                    return Ok(V::None);
                }
                if flag == s.c("FLAG_CARGO") && !same {
                    let free = s.type_info(s.ship_type(target.clone())?)["capacity"].float()
                        - s.cargo_used(target.clone())?;
                    let needed =
                        s.type_info(stored["typeID"].clone())["volume"].float() * qty as f64;
                    if needed > free {
                        return Err(Error::User(
                            "NotEnoughCargoSpace".into(),
                            d!["volume"=>needed,"free"=>free],
                        ));
                    }
                }
                let (added, changed) = s.transaction(|s| {
                    let stack = if !stored["singleton"].truth() && !same {
                        s.find_stack(
                            c.char_id(),
                            target.clone(),
                            stored["typeID"].clone(),
                            flag.clone(),
                        )?
                    } else {
                        V::None
                    };
                    let mut added = vec![];
                    let mut changed = vec![];
                    if !stack.is_none() {
                        s.quantity(stack["itemID"].clone(), stack["quantity"].int() + qty)?;
                        s.quantity(a[0].clone(), stored["quantity"].int() - qty)?;
                        changed.extend([stack["itemID"].clone(), a[0].clone()]);
                    } else if qty == stored["quantity"].int() {
                        s.exec(
                            "UPDATE inventory SET \"locationID\"=$1,flag=$2 WHERE \"itemID\"=$3",
                            &[target, flag, a[0].clone()],
                        )?;
                        changed.push(a[0].clone());
                    } else {
                        let new_id =
                            V::from(s.next_id("inventory", "itemID", s.c("ITEM_ID_BASE").int())?);
                        let mut new = stored.clone();
                        new.set("itemID", new_id.clone());
                        new.set("locationID", target);
                        new.set("flag", flag);
                        new.set("quantity", qty);
                        s.quantity(a[0].clone(), stored["quantity"].int() - qty)?;
                        s.insert("inventory", &new, false)?;
                        added.push(new_id);
                        changed.push(a[0].clone());
                    }
                    Ok((added, changed))
                })?;
                self.notify_inventory(c, &added, &changed, &[], true, false)?;
                Ok(V::None)
            }
            _ => Err(format!("inventory.{method} is not implemented by this POC server").into()),
        }
    }
    fn dogma(&mut self, c: &mut Client, method: &str, a: &V) -> Result {
        match method {
            "ShipGetEffects" => Ok(d![]),
            "StationGetEffects" => Ok(l![]),
            "ShipGetAttributes" => Ok(t![
                self.state
                    .ship_attributes(a[0].or(c.ship()), c.session["solarsystemid"].clone())?,
                eve_time()
            ]),
            "ItemGetEffects" => {
                let item = self.state.item(a[0].clone())?;
                if item.is_none() {
                    return Err("no such item to describe".into());
                }
                Ok(l![self.state.inventory_row(&item), d![], d![]])
            }
            "GetAttributes" => {
                let attrs = self
                    .state
                    .systems(c.ship())?
                    .into_iter()
                    .find(|s| s["itemID"] == a[0])
                    .map(|s| s["attributes"].clone())
                    .unwrap_or(d![]);
                Ok(t![attrs, eve_time()])
            }
            "GetTargets" => Ok(V::List(c.targets.iter().map(|id| (*id).into()).collect())),
            "GetTargeters" => Ok(
                if c.rat.as_ref().is_some_and(Rat::alive) && !c.targets.is_empty() {
                    l![RAT_ID]
                } else {
                    l![]
                },
            ),
            "AddTarget" => {
                let rat = c
                    .rat
                    .as_ref()
                    .ok_or("there is nothing to lock -- you are not in space")?;
                if a[0].int() != RAT_ID {
                    return Err(user("The only lockable target is the Pirate Frigate."));
                }
                if !rat.alive() {
                    return Err(user("That wreck is already gone."));
                }
                if c.targets.insert(RAT_ID) {
                    c.broadcast("OnTarget", t!["add", RAT_ID], true)?;
                    c.broadcast("OnTarget", t!["otheradd", RAT_ID], true)?;
                    self.rat_tick(c, true)?;
                }
                Ok(V::None)
            }
            "RemoveTarget" => {
                if c.targets.remove(&a[0].int()) {
                    if let Some(r) = &mut c.rat {
                        r.due = None;
                    }
                    c.broadcast(
                        "OnTarget",
                        t!["lost", a[0].clone(), "you released the lock"],
                        true,
                    )?;
                    c.broadcast("OnTarget", t!["otherlost", a[0].clone(), V::None], true)?;
                }
                Ok(V::None)
            }
            "ClearTargets" => {
                let targets = std::mem::take(&mut c.targets);
                if let Some(r) = &mut c.rat {
                    r.due = None;
                }
                if !targets.is_empty() {
                    c.broadcast("OnTarget", t!["clear"], true)?;
                    for id in targets {
                        c.broadcast("OnTarget", t!["otherlost", id, V::None], true)?;
                    }
                }
                Ok(V::None)
            }
            _ => Err(format!("dogmaIM.{method} is not implemented by this POC server").into()),
        }
    }
    pub fn push_state(&self, c: &mut Client, id: &str, defer: bool) -> Result<()> {
        let b = c.objects.get_mut(id).ok_or("ballpark is gone")?;
        b.epoch = Instant::now();
        b.state_sent = true;
        let state = destiny::undock(&self.state, b.context["ship_id"].clone(), c.rat.as_ref());
        c.broadcast("OnDestinyUpdate", t![state], defer)
    }
    fn park(&mut self, c: &mut Client, id: &str, method: &str, a: &V) -> Result {
        if method == "UpdateStateRequest" {
            self.push_state(c, id, true)?;
            return Ok(V::None);
        }
        let b = c.objects.get(id).ok_or("unknown ballpark")?;
        let epoch = b.epoch;
        if !b.state_sent
            || c.ship().is_none()
            || !c.session["stationid"].is_none()
            || c.ship() != b.context["ship_id"]
        {
            return Err("you are not flying this ballpark's ship".into());
        }
        let mut args = vec![c.ship()];
        let mut name = method;
        match method {
            "GotoDirection" | "GotoPoint" => {
                let xyz = [a[0].float(), a[1].float(), a[2].float()];
                if xyz.iter().any(|n| !n.is_finite()) {
                    return Err(user("Coordinates must be finite."));
                }
                if method == "GotoDirection" && xyz == [0., 0., 0.] {
                    return Err("a direction of (0, 0, 0) is not a direction".into());
                }
                args.extend(xyz.map(V::from));
            }
            "SetSpeedFraction" => args.push(a[0].or(1.0.into()).float().clamp(0., 1.).into()),
            "Stop" => (),
            "FollowBall" | "Orbit" | "WarpToItem" => {
                if a[0] == c.ship() {
                    return Err(user("You cannot approach your own ship."));
                }
                let target = destiny::layout(&self.state, c.ship(), c.rat.as_ref()).key(&a[0]);
                if target.is_none() {
                    return Err(user("There is nothing out there with that ID."));
                }
                if method == "WarpToItem" {
                    name = "WarpTo";
                    let xyz = [target[0].float(), target[1].float(), target[2].float()];
                    let radius = target[3].float();
                    let length = xyz.iter().map(|n| n * n).sum::<f64>().sqrt();
                    let arrival = if length == 0. {
                        [0., 0., radius + 15000.]
                    } else {
                        xyz.map(|n| n * (length + radius + 15000.) / length)
                    };
                    args.extend(arrival.map(V::from));
                } else {
                    args.push(a[0].clone());
                    args.push(a[1].float().into());
                }
            }
            "StargateJump" => {
                return Err(user(
                    "This server has no stargates -- there is one solar system and no way out of it.",
                ));
            }
            "Dock" | "TurboDock" => {
                return Err(user(
                    "This server cannot dock you again yet. Undocking is a one-way trip; reconnect to start docked.",
                ));
            }
            "GangWarp" | "GangRegroup" => return Err(user("This server has no gangs.")),
            _ => {
                return Err(
                    format!("ballpark.{method} is not implemented by this POC server").into(),
                );
            }
        }
        c.broadcast(
            "OnDestinyUpdate",
            t![destiny::action(epoch, name, args)],
            true,
        )?;
        Ok(V::None)
    }
    pub fn rat_tick(&self, c: &mut Client, defer: bool) -> Result<()> {
        // A timer fires once. Only an active attack schedules its successor;
        // releasing a ballpark must not leave an overdue timer polling forever.
        if let Some(rat) = &mut c.rat {
            rat.due = None;
        }
        let Some(epoch) = c
            .ballpark
            .as_ref()
            .and_then(|id| c.objects.get(id))
            .map(|b| b.epoch)
        else {
            return Ok(());
        };
        let Some(rat) = &mut c.rat else {
            return Ok(());
        };
        if !rat.alive() || !c.targets.contains(&RAT_ID) {
            return Ok(());
        }
        let killed = rat.hit();
        let damage = rat.damage(&self.state);
        c.broadcast(
            "OnDestinyUpdate",
            t![destiny::action(
                epoch,
                "OnDamageStateChange",
                vec![RAT_ID.into(), damage]
            )],
            defer,
        )?;
        if killed {
            c.targets.remove(&RAT_ID);
            c.broadcast(
                "OnDestinyUpdate",
                t![destiny::action(
                    epoch,
                    "OnSpecialFX",
                    vec![
                        RAT_ID.into(),
                        V::None,
                        V::None,
                        V::None,
                        "effects.Explosion".into(),
                        0.into(),
                        1.into(),
                        0.into(),
                        (-1).into(),
                        V::None
                    ]
                )],
                defer,
            )?;
            c.broadcast(
                "OnDestinyUpdate",
                t![destiny::action(
                    epoch,
                    "RemoveBall",
                    vec![RAT_ID.into(), (-1).into()]
                )],
                defer,
            )?;
        }
        Ok(())
    }
    pub fn tick(&mut self) -> Result<()> {
        let ids = self
            .clients
            .iter()
            .filter(|(_, c)| {
                c.rat
                    .as_ref()
                    .and_then(|r| r.due)
                    .is_some_and(|due| due <= Instant::now())
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in ids {
            let mut c = self.clients.remove(&id).unwrap();
            let result = (|| -> Result<()> {
                if c.rat.as_ref().is_some_and(Rat::alive) {
                    self.rat_tick(&mut c, false)?;
                } else if let Some(epoch) = c
                    .ballpark
                    .as_ref()
                    .and_then(|id| c.objects.get(id))
                    .map(|b| b.epoch)
                {
                    c.rat = Some(Rat::default());
                    let (ball, slim) = destiny::rat_objects(&self.state);
                    let damage = c.rat.as_ref().unwrap().damage(&self.state);
                    c.broadcast(
                        "OnDestinyUpdate",
                        t![destiny::action(
                            epoch,
                            "AddBalls",
                            vec![t![l![t![ball, slim]], d![RAT_ID=>damage]]]
                        )],
                        false,
                    )?;
                } else if let Some(r) = &mut c.rat {
                    r.due = None;
                }
                Ok(())
            })();
            self.clients.insert(id, c);
            result?;
        }
        Ok(())
    }
    fn chat_object(&mut self, c: &mut Client, method: &str, ctx: &V, a: &V) -> Result {
        let room = (ctx[0].int(), ctx[1].int());
        match method {
            "List" => {
                let mut lines = vec![];
                if c.rooms.contains(&room) {
                    lines.push(self.member(c)?.state()["line"].seq().to_vec());
                }
                for other in self.clients.values() {
                    if other.rooms.contains(&room) {
                        lines.push(self.member(other)?.state()["line"].seq().to_vec());
                    }
                }
                Ok(rowset(self.state.c("CHANNEL_MEMBER_HEADER"), lines))
            }
            "Speak" => {
                self.tell_room(
                    c,
                    room,
                    "OnChatSpeak",
                    t![room.0, room.1, c.char_id(), a[0].clone()],
                )?;
                Ok(V::None)
            }
            "Typing" => {
                self.tell_room(c, room, "OnChatTyping", t![room.0, room.1, c.char_id()])?;
                Ok(V::None)
            }
            "Activate" | "DeActivate" => Ok(V::None),
            "Leave" => {
                self.leave_room(c, room)?;
                Ok(V::None)
            }
            "InviteCharacters" => {
                let channel = a[1].or(room.1.into());
                let name = self
                    .channels
                    .iter()
                    .find(|(_, id)| **id == channel.int())
                    .map(|(name, _)| V::from(name.clone()))
                    .unwrap_or(V::None);
                let invitation = self.state.data_row(
                    "CHANNEL_HEADER",
                    vec![channel, room.0.into(), name, 1.into(), d![]],
                );
                let inviter = c.char_id();
                if a[0].seq().contains(&inviter) {
                    c.broadcast(
                        "OnChatInvite",
                        t![inviter.clone(), invitation.clone()],
                        false,
                    )?;
                }
                for other in self.clients.values_mut() {
                    if a[0].seq().contains(&other.char_id()) {
                        other.broadcast(
                            "OnChatInvite",
                            t![inviter.clone(), invitation.clone()],
                            false,
                        )?;
                    }
                }
                Ok(V::None)
            }
            _ => Err(format!("chat.{method} is not implemented by this POC server").into()),
        }
    }
}
