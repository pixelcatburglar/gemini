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
            if kind == "park" && c.ballpark.as_deref() == Some(id) {
                if let Some(f) = c.flight.as_mut() {
                    f.update();
                    self.state.save_flight(c.session["shipid"].clone(), f)?;
                }
                self.space_event(c, "RemoveBall", vec![c.ship(), (-1).into()], true)?;
                c.ballpark = None;
                c.targets.clear();
            }
            c.objects.remove(id);
            return Ok(V::None);
        }
        match kind.as_str() {
            "inventory" => self.inventory(c, id, method, &context, &a),
            "ship" => {
                match method {
                    "Board" => self.board(c, a[0].clone())?,
                    "Eject" => self.board(c, V::None)?,
                    "Undock" => self.undock_ship(c)?,
                    "Jettison" => {
                        return Err(user("Cargo jettisoning is not implemented yet."));
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
        if c.char_id().is_none() || !s.owns(c.account.clone(), c.char_id())? {
            return Err(user("Select your character first."));
        }
        if !ctx["owner_id"].is_none() && ctx["owner_id"] != c.char_id() {
            return Err(user("This inventory belongs to another owner."));
        }
        if !ctx["item_id"].is_none()
            && ctx["item_id"] != c.session["stationid"]
            && s.item(ctx["item_id"].clone())?["ownerID"] != c.char_id()
        {
            return Err(user("This inventory belongs to another owner."));
        }
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
                if item == c.ship() || s.hangar_ids(c.char_id())?.contains(&item) {
                    return s.ship_item(item, c.session["locationid"].clone());
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
                s.require_station(c)?;
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
                if stored["itemID"] == c.ship() {
                    return Err(user("Eject before moving your active ship."));
                }
                if target == stored["itemID"] {
                    return Err(user("An item cannot contain itself."));
                }
                if stored["locationID"] != c.session["stationid"]
                    && s.item(stored["locationID"].clone())?["ownerID"] != c.char_id()
                {
                    return Err(user("That item is not accessible from this station."));
                }
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
            "GetTargeters" => Ok(if self.rat.alive() && !c.targets.is_empty() {
                l![RAT_ID]
            } else {
                l![]
            }),
            "AddTarget" => {
                self.state.require_space(c)?;
                let rat = &self.rat;
                let position = c.flight.as_ref().ok_or("no flight state")?.position;
                if crate::space::length(crate::space::sub(position, [6000., 0., 9000.])) > 100000. {
                    return Err(user("That target is out of locking range."));
                }
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
        self.state.require_space(c)?;
        if c.ballpark.as_deref() != Some(id) {
            return Err(user("This ballpark is no longer active."));
        }
        if let Some(f) = c.flight.as_mut() {
            f.update();
        }
        let b = c.objects.get_mut(id).ok_or("ballpark is gone")?;
        if b.context["ship_id"] != c.session["shipid"] {
            return Err(user("This ballpark belongs to another ship."));
        }
        b.state_sent = true;
        let epoch = b.epoch;
        let stamp = destiny::stamp(epoch);
        let (mut balls, mut slims) = destiny::objects(&self.state, Some(&self.rat));
        let (ball, slim) = destiny::player_objects(&self.state, c)?;
        balls.push(ball);
        slims.push(slim);
        let mut damage = d![];
        let hp = self.state.data["catalog.SHIP_DOGMA_STATS"].key(&self.state.ship_type(c.ship())?)
            ["hp"]
            .float()
            .max(1.);
        damage.set(
            c.ship(),
            l![
                t![1., 1e12],
                1.,
                (1. - self.state.item_damage(c.ship())? / hp).clamp(0., 1.)
            ],
        );
        if let Some(r) = Some(&self.rat).filter(|r| r.alive()) {
            damage.set(RAT_ID, r.damage(&self.state));
        }
        for other in self.clients.values().filter(|o| {
            !o.closed
                && o.flight.is_some()
                && o.session["stationid"].is_none()
                && o.session["solarsystemid"] == c.session["solarsystemid"]
        }) {
            let (ball, slim) = destiny::player_objects(&self.state, other)?;
            balls.push(ball);
            slims.push(slim);
        }
        c.broadcast(
            "OnDestinyUpdate",
            t![l![t![
                stamp,
                t![
                    "SetState",
                    t![
                        V::List(balls),
                        V::List(slims),
                        stamp,
                        c.ship(),
                        V::None,
                        damage
                    ]
                ]
            ]]],
            defer,
        )?;
        let mut replay = vec![];
        for pilot in std::iter::once(&*c)
            .chain(self.clients.values())
            .filter(|o| {
                o.flight.is_some()
                    && !o.closed
                    && o.session["solarsystemid"] == c.session["solarsystemid"]
                    && o.session["stationid"].is_none()
            })
        {
            let (name, mut args) = pilot.flight.as_ref().unwrap().command();
            args.insert(0, pilot.ship());
            replay.push(destiny::action(epoch, name, args));
        }
        for packet in replay {
            c.broadcast("OnDestinyUpdate", t![packet], defer)?;
        }
        Ok(())
    }
    fn park(&mut self, c: &mut Client, id: &str, method: &str, a: &V) -> Result {
        use crate::space::{self, Motion};
        self.state.require_space(c)?;
        let b = c.objects.get(id).ok_or("unknown ballpark")?;
        if c.ballpark.as_deref() != Some(id) || c.ship() != b.context["ship_id"] {
            return Err(user("This ballpark is no longer active."));
        }
        if method == "UpdateStateRequest" {
            self.push_state(c, id, true)?;
            return Ok(V::None);
        }
        if !b.state_sent {
            return Err(user("Wait for the ballpark to load."));
        }
        let ship = c.ship();
        let mut flight = c.flight.clone().ok_or("missing flight state")?;
        flight.update();
        if matches!(flight.motion, Motion::Warp(_)) {
            return Err(user("Wait until the ship leaves warp."));
        }
        let mut args = vec![ship.clone()];
        let mut name = method;
        let mut layout = destiny::layout(&self.state, Some(&self.rat));
        for other in self.clients.values().filter(|o| {
            o.session["solarsystemid"] == c.session["solarsystemid"]
                && o.session["stationid"].is_none()
        }) {
            if let Some(f) = &other.flight {
                let mut f = f.clone();
                f.update();
                layout.set(
                    other.ship(),
                    t![f.position[0], f.position[1], f.position[2], f.radius],
                );
            }
        }
        flight.pending_dock = None;
        match method {
            "GotoDirection" | "GotoPoint" => {
                let xyz = space::coordinates(a)?;
                if method == "GotoDirection" && space::length(xyz) == 0. {
                    return Err(user("A direction cannot be zero."));
                }
                flight.motion = if method == "GotoDirection" {
                    Motion::Direction(space::unit(xyz))
                } else {
                    Motion::Point(xyz)
                };
                args.extend(xyz.map(V::from));
            }
            "SetSpeedFraction" => {
                let fraction = a[0].float();
                if !matches!(a[0], V::Int(_) | V::Float(_)) || !fraction.is_finite() {
                    return Err(user("Speed must be finite."));
                }
                flight.speed = fraction.clamp(0., 1.);
                args.push(flight.speed.into());
            }
            "Stop" => {
                flight.motion = Motion::Stopped;
                flight.speed = 0.;
            }
            "FollowBall" | "Orbit" | "WarpToItem" | "Dock" | "TurboDock" => {
                if a[0] == ship {
                    return Err(user("Select another object."));
                }
                let target = layout.key(&a[0]);
                if target.is_none() {
                    return Err(user("That object is not in your solar system."));
                }
                let center = [target[0].float(), target[1].float(), target[2].float()];
                let radius = target[3].float();
                if matches!(method, "Dock" | "TurboDock") {
                    if a[0] != self.state.c("STATION_ID") {
                        return Err(user("That object is not a docking station."));
                    }
                    if space::length(space::sub(flight.position, center))
                        <= radius + flight.radius + self.state.p("maxDockingDistance").float()
                    {
                        c.flight = Some(flight);
                        self.dock_ship(c, a[0].clone())?;
                        return Ok(V::None);
                    }
                    flight.pending_dock = Some(a[0].clone());
                    flight.motion = Motion::Follow {
                        target: a[0].clone(),
                        center,
                        radius,
                        distance: 0.,
                        orbit: false,
                    };
                    flight.speed = 1.;
                    name = "FollowBall";
                    args.extend([a[0].clone(), 0.0.into()]);
                } else if method == "WarpToItem" {
                    if space::length(space::sub(flight.position, center)) - radius - flight.radius
                        < self.state.p("minWarpDistance").float()
                    {
                        return Err(user("The destination is too close to warp to."));
                    }
                    let destination =
                        space::arrival(flight.position, center, radius + flight.radius + 15000.);
                    flight.motion = Motion::Warp(destination);
                    name = "WarpTo";
                    args.extend(destination.map(V::from));
                } else {
                    let distance = a[1].float();
                    if !matches!(a[1], V::Int(_) | V::Float(_))
                        || !distance.is_finite()
                        || !(0.0..=1e12).contains(&distance)
                    {
                        return Err(user("Distance must be finite and nonnegative."));
                    }
                    flight.motion = Motion::Follow {
                        target: a[0].clone(),
                        center,
                        radius,
                        distance,
                        orbit: method == "Orbit",
                    };
                    args.extend([a[0].clone(), distance.into()]);
                }
            }
            "StargateJump" => return Err(user("Stargate travel is not implemented yet.")),
            "GangWarp" | "GangRegroup" => return Err(user("You are not in a gang.")),
            _ => return Err(format!("ballpark.{method} is not implemented").into()),
        }
        if matches!(
            method,
            "GotoDirection" | "GotoPoint" | "FollowBall" | "Orbit"
        ) && flight.speed == 0.
        {
            flight.speed = 1.;
        }
        self.state.save_flight(ship.clone(), &flight)?;
        let speed = flight.speed;
        c.flight = Some(flight);
        if !matches!(method, "SetSpeedFraction" | "WarpToItem") {
            self.space_event(c, "SetSpeedFraction", vec![ship, speed.into()], true)?;
        }
        self.space_event(c, name, args, true)?;
        Ok(V::None)
    }
    pub fn rat_tick(&mut self, c: &mut Client, defer: bool) -> Result<()> {
        if !self.rat.alive()
            || c.ballpark.is_none()
            || !c.session["stationid"].is_none()
            || !c.targets.contains(&RAT_ID)
        {
            return Ok(());
        }
        let position = c.flight.as_ref().ok_or("no flight state")?.position;
        if crate::space::length(crate::space::sub(position, [6000., 0., 9000.])) > 100000. {
            c.targets.remove(&RAT_ID);
            c.broadcast("OnTarget", t!["lost", RAT_ID, "Target out of range"], defer)?;
            return Ok(());
        }
        let killed = self.rat.hit();
        self.space_event(
            c,
            "OnDamageStateChange",
            vec![RAT_ID.into(), self.rat.damage(&self.state)],
            defer,
        )?;
        if killed {
            let current = c.id;
            for pilot in std::iter::once(&mut *c).chain(self.clients.values_mut()) {
                if pilot.targets.remove(&RAT_ID) {
                    pilot.broadcast(
                        "OnTarget",
                        t!["lost", RAT_ID, "Target destroyed"],
                        pilot.id == current && defer,
                    )?;
                    pilot.broadcast(
                        "OnTarget",
                        t!["otherlost", RAT_ID, V::None],
                        pilot.id == current && defer,
                    )?;
                }
            }
            self.space_event(
                c,
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
                    V::None,
                ],
                defer,
            )?;
            self.space_event(c, "RemoveBall", vec![RAT_ID.into(), (-1).into()], defer)?;
        }
        Ok(())
    }
    pub fn tick(&mut self) -> Result<()> {
        self.tick_flights()?;
        if !self.rat.due.is_some_and(|due| due <= Instant::now()) {
            return Ok(());
        }
        self.rat.due = None;
        if !self.rat.alive() {
            self.rat = Rat::default();
            let (ball, slim) = destiny::rat_objects(&self.state);
            let damage = self.rat.damage(&self.state);
            for c in self
                .clients
                .values_mut()
                .filter(|c| !c.closed && c.session["stationid"].is_none())
            {
                if let Some(b) = c.ballpark.as_ref().and_then(|id| c.objects.get(id)) {
                    c.broadcast(
                        "OnDestinyUpdate",
                        t![destiny::action(
                            b.epoch,
                            "AddBalls",
                            vec![t![
                                l![t![ball.clone(), slim.clone()]],
                                d![RAT_ID=>damage.clone()]
                            ]]
                        )],
                        false,
                    )?;
                }
            }
        } else {
            let ids: Vec<_> = self
                .clients
                .iter()
                .filter(|(_, c)| {
                    !c.closed
                        && c.ballpark.is_some()
                        && c.targets.contains(&RAT_ID)
                        && c.session["stationid"].is_none()
                })
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                let mut c = self.clients.remove(&id).unwrap();
                let result = self.rat_tick(&mut c, false);
                self.clients.insert(id, c);
                result?;
                if !self.rat.alive() {
                    break;
                }
            }
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
