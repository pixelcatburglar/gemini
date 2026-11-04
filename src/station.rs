//! Station contracts recovered from the Gemini client. All mutations are atomic.
use crate::{
    d,
    game::Client,
    l,
    market::Changes,
    pickle::Value as V,
    state::{Error, Result, State, user},
    t,
    value::row,
};

pub(crate) const GAMEPLAY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS item_damage (
 "shipID" BIGINT NOT NULL REFERENCES inventory("itemID") ON DELETE CASCADE,
 "itemID" BIGINT NOT NULL, damage DOUBLE PRECISION NOT NULL DEFAULT 0
 CHECK (damage >= 0 AND damage < 'Infinity'::float8), PRIMARY KEY("shipID","itemID"));
CREATE TABLE IF NOT EXISTS reprocessing_recipes (
 "typeID" BIGINT NOT NULL, "materialTypeID" BIGINT NOT NULL,
 portion BIGINT NOT NULL CHECK(portion > 0), quantity BIGINT NOT NULL CHECK(quantity > 0),
 refinable BOOLEAN NOT NULL DEFAULT false, PRIMARY KEY("typeID","materialTypeID"),
 CHECK("typeID" <> "materialTypeID"));
CREATE TABLE IF NOT EXISTS ship_position (
 "shipID" BIGINT PRIMARY KEY REFERENCES inventory("itemID") ON DELETE CASCADE,
 x DOUBLE PRECISION NOT NULL, y DOUBLE PRECISION NOT NULL, z DOUBLE PRECISION NOT NULL,
 CHECK(abs(x)<1e18 AND abs(y)<1e18 AND abs(z)<1e18));
"#;

pub(crate) fn signature(service: &str, method: &str) -> V {
    let params: &[&str] = match (service, method) {
        ("repair", "GetDamageReport") => &["shipID"],
        ("repair", "RepairShip") => &["shipID", "amount"],
        ("repair", "RepairModule" | "DamageModule") => &["moduleID", "amount"],
        ("reprocessing", "GetOptionsForItemTypes") => &["itemTypes"],
        ("reprocessing", "GetQuote" | "Reprocess") => &["itemID"],
        _ => return V::None,
    };
    d!["params"=>V::List(params.iter().map(|p|t![*p,0,V::None]).collect()),"varargs"=>0,"keywords"=>0]
}
fn positive_id(v: &V) -> Result<i64> {
    v.as_i64()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| user("A valid item or type ID is required."))
}
fn amount(v: &V) -> Result<f64> {
    let n = v.float();
    if !matches!(v, V::Int(_) | V::Float(_)) || !n.is_finite() || n <= 0. {
        return Err(user("Amount must be a finite positive number."));
    }
    Ok(n)
}
pub(crate) fn field(v: &V, name: &str) -> V {
    let state = v.state();
    state["header"]
        .seq()
        .iter()
        .position(|k| k.text() == name)
        .map(|i| state["line"][i].clone())
        .unwrap_or(V::None)
}
impl State {
    pub(crate) fn migrate_gameplay(&mut self) -> Result<()> {
        self.transaction(|s| {
            s.db.batch(GAMEPLAY_SCHEMA)?;
            if s.one("SELECT value FROM meta WHERE key='damage_key_version'",&[])?["value"].is_none() {
                s.db.batch("ALTER TABLE item_damage DROP CONSTRAINT IF EXISTS item_damage_pkey; ALTER TABLE item_damage ADD PRIMARY KEY(\"shipID\",\"itemID\"); INSERT INTO meta(key,value) VALUES('damage_key_version','1');")?;
            }
            if s.one("SELECT value FROM meta WHERE key='gameplay_schema_version'",&[])?["value"].text()=="1" {return Ok(());}
            // The recovered catalog has no manufacturing bill of materials.
            // Seed an explicit, editable starter economy: recover half the
            // catalog value as Tritanium. Never pretend these are historical yields.
            let mineral=s.c("TYPES").seq().iter().find(|r|r[2].text()=="Tritanium").map(|r|r[0].clone()).unwrap_or(20221.into());
            let mineral_price=s.price(&mineral).max(1);
            for typ in s.c("TYPES").seq() {
                let info=s.type_info(typ[0].clone());
                let name=info["typeName"].or(info["name"].clone()).text();
                let batch=s.data["catalog.ORE_BATCH"].seq().iter().find(|r|r[0].text()==name).map(|r|r[1].int());
                let category=s.category(typ[0].clone()).int();
                if batch.is_none() && ![6,7,8].contains(&category) {continue;}
                let portion=batch.unwrap_or(1);
                let quantity=s.price(&typ[0]).saturating_mul(portion)/mineral_price/2;
                if quantity<=0 || typ[0]==mineral {continue;}
                s.exec("INSERT INTO reprocessing_recipes VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",&[typ[0].clone(),mineral.clone(),portion.into(),quantity.into(),i32::from(batch.is_some()).into()])?;
            }
            s.exec("INSERT INTO meta(key,value) VALUES('gameplay_schema_version','1') ON CONFLICT(key) DO UPDATE SET value=excluded.value",&[])?;
            Ok(())
        })?;
        let changed=self.transaction(|s| {
            if !s.one("SELECT value FROM meta WHERE key='destiny_catalog_version'",&[])?["value"].is_none(){return Ok(false);}
            s.db.batch(r#"
UPDATE game_constants c SET "constantValue"=g."groupID"
FROM game_groups g WHERE c."constantID" LIKE 'group%'
AND regexp_replace(lower(substring(c."constantID" FROM 6)), '[^a-z0-9]', '', 'g')=regexp_replace(lower(g."groupName"),'[^a-z0-9]','','g');
UPDATE game_constants c SET "constantValue"=g."categoryID"
FROM game_categories g WHERE c."constantID" LIKE 'category%'
AND lower(substring(c."constantID" FROM 9))=lower(g."categoryName");
UPDATE game_constants SET "constantValue"=(SELECT "groupID" FROM game_groups WHERE "groupName"='Ore') WHERE "constantID"='groupAsteroid';
UPDATE game_constants SET "constantValue"=CASE "constantID" WHEN 'minWarpDistance' THEN 150000 WHEN 'maxDockingDistance' THEN 2500 WHEN 'maxStargateJumpingDistance' THEN 2500 WHEN 'dgmTauConstant' THEN 10000 END WHERE "constantID" IN ('minWarpDistance','maxDockingDistance','maxStargateJumpingDistance','dgmTauConstant');
UPDATE meta SET value=((value::bigint)+1)::text WHERE key='bulk_version';
INSERT INTO meta(key,value) VALUES('destiny_catalog_version','1');
"#)?;
            Ok(true)
        })?;
        if changed {
            self.data = crate::reference::load(&self.db)?;
        }
        Ok(())
    }
    pub(crate) fn require_station(&self, c: &Client) -> Result {
        if c.account.is_none()
            || c.char_id().is_none()
            || !self.owns(c.account.clone(), c.char_id())?
        {
            return Err(user("Select one of your characters first."));
        }
        let station = self.location(c.char_id())?["stationID"].clone();
        if station.is_none() || c.session["stationid"] != station {
            return Err(user("You must be docked to use this station service."));
        }
        Ok(station)
    }
    fn station_item(&self, c: &Client, id: &V) -> Result {
        positive_id(id)?;
        let station = self.require_station(c)?;
        let item = self.item(id.clone())?;
        if item.is_none()
            || item["ownerID"] != c.char_id()
            || item["locationID"] != station
            || item["flag"] != self.c("FLAG_HANGAR")
        {
            return Err(user("That item is not available in your station hangar."));
        }
        Ok(item)
    }
    pub(crate) fn item_damage(&self, id: V) -> Result<f64> {
        self.damage_for(id.clone(), id)
    }
    pub(crate) fn damage_for(&self, ship: V, id: V) -> Result<f64> {
        Ok(self.one(
            "SELECT damage FROM item_damage WHERE \"shipID\"=$1 AND \"itemID\"=$2",
            &[ship, id],
        )?["damage"]
            .float())
    }
    pub(crate) fn set_damage(&self, ship: V, id: V, damage: f64) -> Result<()> {
        self.exec("INSERT INTO item_damage VALUES($1,$2,$3) ON CONFLICT(\"shipID\",\"itemID\") DO UPDATE SET damage=excluded.damage",&[ship,id,damage.into()])?;
        Ok(())
    }
    pub(crate) fn repair_report(&self, c: &Client, id: &V) -> Result {
        let ship = self.station_item(c, id)?;
        if self.category(ship["typeID"].clone()) != self.c("CATEGORY_SHIP") {
            return Err(user("Select a ship to repair."));
        }
        let mut items = vec![(
            ship.clone(),
            self.data["catalog.SHIP_DOGMA_STATS"].key(&ship["typeID"])["hp"]
                .float()
                .max(1.),
        )];
        for module in self.systems(id.clone())? {
            let hp = module["attributes"]["hp"].float();
            if hp > 0. {
                items.push((module, hp));
            }
        }
        let mut quotes = vec![];
        for (item, hp) in items {
            let damage = self.damage_for(id.clone(), item["itemID"].clone())?.min(hp);
            // One ISK per HP; the client displays int(damage) * this rate.
            quotes.push(row(
                l![
                    "itemID",
                    "typeID",
                    "damage",
                    "maxHealth",
                    "costToRepairOneUnitOfDamage"
                ],
                vec![
                    item["itemID"].clone(),
                    item["typeID"].clone(),
                    damage.into(),
                    hp.into(),
                    1.0.into(),
                ],
            ));
        }
        Ok(row(l!["quote"], vec![V::List(quotes)]))
    }
    pub(crate) fn repair(
        &mut self,
        c: &Client,
        id: &V,
        budget: &V,
        whole: bool,
        damage: bool,
    ) -> Result {
        positive_id(id)?;
        let budget = amount(budget)?;
        self.transaction(|s| {
            let mut ship = id.clone();
            if !whole && s.item(id.clone())?.is_none() {
                ship = V::None;
                for candidate in s.hangar_ids(c.char_id())? {
                    if s.systems(candidate.clone())?
                        .iter()
                        .any(|m| m["itemID"] == *id)
                    {
                        ship = candidate;
                        break;
                    }
                }
                if ship.is_none() {
                    return Err(user("That module is not in your hangar."));
                }
            }
            let report = s.repair_report(c, &ship)?;
            let quotes = field(&report, "quote");
            let mut remaining = budget;
            let mut spent = 0.;
            for q in quotes
                .seq()
                .iter()
                .filter(|q| whole || field(q, "itemID") == *id)
            {
                let item = field(q, "itemID");
                let old = field(q, "damage").float();
                if damage {
                    s.set_damage(
                        ship.clone(),
                        item,
                        (old + budget).min(field(q, "maxHealth").float()),
                    )?;
                } else {
                    let repaired = old.min(remaining).floor();
                    s.set_damage(ship.clone(), item, old - repaired)?;
                    spent += repaired;
                    remaining -= repaired;
                }
            }
            if !damage && spent > s.balance(c.char_id())? {
                return Err(user("You do not have enough ISK for those repairs."));
            }
            if spent > 0. {
                s.money(c.char_id(), -spent)?;
            }
            Ok(V::None)
        })
    }
    pub(crate) fn reprocessing_options(&self, types: &V) -> Result {
        let ids: Vec<_> = match types {
            V::Dict(p) => p.iter().map(|(k, _)| k).collect(),
            V::List(v) | V::Tuple(v) => v.iter().collect(),
            _ => return Err(user("Expected item types.")),
        };
        if ids.len() > 4096 {
            return Err(user("Too many item types requested."));
        }
        let recipes = self.query(
            "SELECT DISTINCT \"typeID\",refinable FROM reprocessing_recipes",
            &[],
        )?;
        let mut out = d![];
        for id in ids {
            positive_id(id)?;
            let recipe = recipes.iter().find(|r| r["typeID"] == *id);
            out.set(
                id.clone(),
                row(
                    l!["typeID", "isRecyclable", "isRefinable"],
                    vec![
                        id.clone(),
                        i32::from(recipe.is_some_and(|r| !r["refinable"].truth())).into(),
                        i32::from(recipe.is_some_and(|r| r["refinable"].truth())).into(),
                    ],
                ),
            );
        }
        Ok(out)
    }
    pub(crate) fn reprocessing_quote(&self, c: &Client, id: &V) -> Result {
        let item = self.station_item(c, id)?;
        if *id == self.location(c.char_id())?["shipID"] {
            return Err(user("Leave the active ship before recycling it."));
        }
        if !self
            .one(
                "SELECT 1 AS n FROM inventory WHERE \"locationID\"=$1 LIMIT 1",
                std::slice::from_ref(id),
            )?
            .is_none()
        {
            return Err(user("Empty this item's contents before recycling it."));
        }
        let recipes = self.query(
            "SELECT * FROM reprocessing_recipes WHERE \"typeID\"=$1 ORDER BY \"materialTypeID\"",
            &[item["typeID"].clone()],
        )?;
        if recipes.is_empty() {
            return Ok(V::None);
        }
        let portion = recipes[0]["portion"].int();
        if recipes.iter().any(|r| r["portion"].int() != portion) {
            return Err("inconsistent reprocessing portions".into());
        }
        let batches = item["quantity"].int() / portion;
        if batches == 0 {
            return Err(Error::User(
                "QuantityLessThanMinimumPortion".into(),
                d!["portion"=>portion],
            ));
        }
        let mut materials = vec![];
        for r in recipes {
            if self.type_info(r["materialTypeID"].clone()).is_none() {
                return Err("unknown reprocessing material".into());
            }
            let total = batches
                .checked_mul(r["quantity"].int())
                .ok_or("mineral quantity overflow")?;
            let fee = total / 20;
            materials.push(row(
                l!["typeID", "client", "station", "unrecoverable"],
                vec![
                    r["materialTypeID"].clone(),
                    (total - fee).into(),
                    fee.into(),
                    0.into(),
                ],
            ));
        }
        Ok(row(
            l!["quantityToProcess", "recoverables"],
            vec![(batches * portion).into(), V::List(materials)],
        ))
    }
    pub(crate) fn reprocess(&mut self, c: &Client, id: &V) -> Result<Changes> {
        self.transaction(|s| {
            let quote=s.reprocessing_quote(c,id)?;
            if quote.is_none(){return Err(user("This item contains no recoverable minerals."));}
            let item=s.item(id.clone())?;
            let left=item["quantity"].int()-field(&quote,"quantityToProcess").int();
            s.quantity(id.clone(),left)?;
            let mut changes=Changes::default();
            if left==0 {changes.removed.push(id.clone());} else {changes.changed.push(id.clone());}
            for material in field(&quote,"recoverables").seq() {
                let typ=field(material,"typeID");
                let n=field(material,"client").int();
                if n<=0 {continue;}
                let stack=s.find_stack(c.char_id(),item["locationID"].clone(),typ.clone(),s.c("FLAG_HANGAR"))?;
                if !stack.is_none() {
                    let quantity=stack["quantity"].int().checked_add(n).ok_or("inventory quantity overflow")?;
                    s.quantity(stack["itemID"].clone(),quantity)?;
                    changes.changed.push(stack["itemID"].clone());
                } else {
                    let new=s.next_id("inventory","itemID",s.c("ITEM_ID_BASE").int())?.max(id.int()+1);
                    s.insert("inventory",&d!["itemID"=>new,"ownerID"=>c.char_id(),"locationID"=>item["locationID"].clone(),"typeID"=>typ,"flag"=>s.c("FLAG_HANGAR"),"quantity"=>n,"singleton"=>0],false)?;
                    changes.added.push(new.into());
                }
            }
            Ok(changes)
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::Connection, game::Game, pickle};
    use std::time::{Duration, Instant};
    struct Cleanup {
        db: Connection,
        owner: i64,
        account: i64,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for sql in [
                "DELETE FROM inventory WHERE \"ownerID\"=$1",
                "DELETE FROM wallet WHERE \"ownerID\"=$1",
                "DELETE FROM character_location WHERE \"characterID\"=$1",
                "DELETE FROM characters WHERE \"characterID\"=$1",
            ] {
                self.db
                    .exec(sql, &[self.owner.into()])
                    .expect("remove gameplay test fixture");
            }
            self.db
                .exec(
                    "DELETE FROM accounts WHERE \"accountID\"=$1",
                    &[self.account.into()],
                )
                .unwrap();
        }
    }
    fn fixture(s: &mut State, url: &str) -> (Client, Cleanup) {
        let (account,owner,ship)=s.transaction(|s|{
            let account=s.next_id("accounts","accountID",700000000)?;
            let owner=s.next_id("characters","characterID",700000000)?;
            let ship=s.next_id("inventory","itemID",700000000)?;
            s.insert("accounts",&d!["accountID"=>account,"username"=>format!("gameplay-test-{account}"),"usernameFold"=>format!("gameplay-test-{account}"),"passwordHash"=>"test","salt"=>"test","created"=>0.],false)?;
            let mut pilot=s.template("seed")["characters"][0].clone();pilot.set("characterID",owner);pilot.set("accountID",account);s.insert("characters",&pilot,false)?;
            let mut location=s.template("seed")["character_location"][0].clone();location.set("characterID",owner);location.set("shipID",ship);s.insert("character_location",&location,false)?;
            s.insert("inventory",&d!["itemID"=>ship,"ownerID"=>owner,"locationID"=>s.c("STATION_ID"),"typeID"=>s.c("SHIP_TYPE_ID"),"flag"=>s.c("FLAG_HANGAR"),"quantity"=>1,"singleton"=>1],false)?;
            s.insert("wallet",&d!["ownerID"=>owner,"balance"=>100.],false)?;
            Ok((account,owner,ship))
        }).unwrap();
        let mut c = Client::new(owner);
        c.account = account.into();
        c.session = s.session_for(owner.into()).unwrap();
        assert_eq!(c.ship(), ship.into());
        (
            c,
            Cleanup {
                db: Connection::open(url).unwrap(),
                owner,
                account,
            },
        )
    }
    fn call(g: &mut Game, c: &mut Client, service: &str, method: &str, args: V) -> V {
        g.dispatch(c, service, method, args, d![]).unwrap()
    }
    fn bind(g: &mut Game, c: &mut Client) -> String {
        let v = call(
            g,
            c,
            "beyonce",
            "MachoBindObject",
            t![c.session["solarsystemid"].clone()],
        );
        if let V::Persistent(id) = v {
            id.text()
        } else {
            panic!("expected bound ballpark")
        }
    }
    fn action(g: &mut Game, c: &mut Client, id: &str, method: &str, args: V) -> V {
        g.dispatch_object(c, id, method, args, d![]).unwrap()
    }
    #[test]
    fn amounts_and_ids_are_strict() {
        for v in [
            V::None,
            (-1).into(),
            0.into(),
            f64::NAN.into(),
            f64::INFINITY.into(),
            "100".into(),
        ] {
            assert!(amount(&v).is_err());
        }
        for v in [V::None, (-1).into(), 1.0.into(), "1".into()] {
            assert!(positive_id(&v).is_err());
        }
        assert_eq!(amount(&10.into()).unwrap(), 10.);
    }
    #[test]
    #[ignore = "requires TEST_DATABASE_URL with the imported catalog"]
    fn postgres_station_and_flight_round_trip() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let mut state = State::open(&url).unwrap();
        let (mut c, _cleanup) = fixture(&mut state, &url);
        let (mut other, _other_cleanup) = fixture(&mut state, &url);
        let mut g = Game::new(state, false);
        assert_eq!(
            g.state.p("groupPlanet"),
            g.state.group(
                g.state
                    .c("TYPES")
                    .seq()
                    .iter()
                    .find(|t| g.state.group(t[0].clone()).int() == 7)
                    .unwrap()[0]
                    .clone()
            )
        );
        assert_eq!(g.state.p("groupAsteroid").int(), 1029);
        assert_eq!(g.state.p("categoryModule").int(), 7);
        assert_eq!(g.state.p("minWarpDistance").int(), 150000);
        let bulk = g.state.data["bulkdata.VERSION"].clone();

        let ship = c.ship();
        let report = call(
            &mut g,
            &mut c,
            "repair",
            "GetDamageReport",
            t![ship.clone()],
        );
        assert!(
            field(&report, "quote")
                .seq()
                .iter()
                .all(|q| field(q, "maxHealth").float() > 0.)
        );
        call(
            &mut g,
            &mut c,
            "repair",
            "DamageModule",
            t![ship.clone(), 150],
        );
        call(&mut g, &mut c, "repair", "RepairShip", t![ship.clone(), 40]);
        assert_eq!(g.state.item_damage(ship.clone()).unwrap(), 110.);
        assert_eq!(g.state.balance(c.char_id()).unwrap(), 60.);
        assert!(
            g.dispatch(&mut c, "repair", "RepairShip", t![ship.clone(), 100], d![])
                .is_err()
        );
        assert_eq!(g.state.item_damage(ship.clone()).unwrap(), 110.); // rollback
        assert_eq!(g.state.balance(c.char_id()).unwrap(), 60.);
        assert!(
            g.dispatch(
                &mut other,
                "repair",
                "GetDamageReport",
                t![ship.clone()],
                d![]
            )
            .is_err()
        );
        let report = g
            .dispatch(
                &mut c,
                "repair",
                "GetDamageReport",
                t![],
                d!["shipID"=>ship.clone()],
            )
            .unwrap();
        let report = pickle::loads(&pickle::dumps(&report).unwrap()).unwrap();
        assert_eq!(field(&field(&report, "quote")[0], "damage").float(), 110.);
        let options = call(
            &mut g,
            &mut c,
            "reprocessing",
            "GetOptionsForItemTypes",
            t![d![20220=>0,587=>0,999999=>0]],
        );
        assert!(field(&options.key(&20220.into()), "isRefinable").truth());
        assert!(field(&options.key(&587.into()), "isRecyclable").truth());
        assert!(!field(&options.key(&999999.into()), "isRecyclable").truth());
        assert_eq!(options.pairs().len(), 3);
        assert!(
            g.dispatch(&mut c, "reprocessing", "GetQuote", t![ship.clone()], d![])
                .is_err()
        );
        let ore = g.state.next_id("inventory", "itemID", 700000000).unwrap();
        g.state.insert("inventory",&d!["itemID"=>ore,"ownerID"=>c.char_id(),"locationID"=>g.state.c("STATION_ID"),"typeID"=>20220,"flag"=>g.state.c("FLAG_HANGAR"),"quantity"=>700,"singleton"=>0],false).unwrap();
        let quote = call(&mut g, &mut c, "reprocessing", "GetQuote", t![ore]);
        assert_eq!(field(&quote, "quantityToProcess").int(), 666);
        let recoverables = field(&quote, "recoverables");
        let received = field(&recoverables[0], "client").int();
        call(&mut g, &mut c, "reprocessing", "Reprocess", t![ore]);
        assert_eq!(g.state.item(ore.into()).unwrap()["quantity"].int(), 34);
        let mineral = g
            .state
            .find_stack(
                c.char_id(),
                g.state.c("STATION_ID"),
                20221.into(),
                g.state.c("FLAG_HANGAR"),
            )
            .unwrap();
        assert_eq!(mineral["quantity"].int(), received);
        assert!(
            g.dispatch(&mut c, "reprocessing", "Reprocess", t![ore], d![])
                .is_err()
        );
        assert_eq!(
            g.state.item(mineral["itemID"].clone()).unwrap()["quantity"].int(),
            received
        );
        g.state.quantity(ore.into(), 333).unwrap();
        call(&mut g, &mut c, "reprocessing", "Reprocess", t![ore]);
        assert!(g.state.item(ore.into()).unwrap().is_none());
        assert!(g.state.item(mineral["itemID"].clone()).unwrap()["quantity"].int() > received);
        // A populated ship cannot be recycled, even after ejecting.
        g.board(&mut c, V::None).unwrap();
        let cargo = g.state.next_id("inventory", "itemID", 700000000).unwrap();
        g.state.insert("inventory",&d!["itemID"=>cargo,"ownerID"=>c.char_id(),"locationID"=>ship.clone(),"typeID"=>20221,"flag"=>g.state.c("FLAG_CARGO"),"quantity"=>1,"singleton"=>0],false).unwrap();
        assert!(
            g.dispatch(&mut c, "reprocessing", "Reprocess", t![ship.clone()], d![])
                .is_err()
        );
        g.state.quantity(cargo.into(), 0).unwrap();
        g.board(&mut c, ship.clone()).unwrap();
        assert!(
            g.dispatch(&mut c, "beyonce", "MachoBindObject", t![30001381], d![])
                .is_err()
        );
        g.undock_ship(&mut c).unwrap();
        assert!(g.state.location(c.char_id()).unwrap()["stationID"].is_none());
        assert_eq!(
            g.state.item(ship.clone()).unwrap()["locationID"],
            c.session["solarsystemid"]
        );
        assert!(
            g.dispatch(&mut c, "repair", "GetDamageReport", t![ship.clone()], d![])
                .is_err()
        );
        let park = bind(&mut g, &mut c);
        assert_eq!(bind(&mut g, &mut c), park); // rebinding does not spawn/reset enemies
        let start = c.flight.as_ref().unwrap().position;
        let epoch = c.objects[&park].epoch;
        action(&mut g, &mut c, &park, "GotoDirection", t![1., 0., 0.]);
        c.flight.as_mut().unwrap().advance(3.);
        action(&mut g, &mut c, &park, "UpdateStateRequest", t![]);
        assert_eq!(c.objects[&park].epoch, epoch);
        assert!(c.flight.as_ref().unwrap().position[0] > start[0] + 100.);
        assert!(
            g.dispatch_object(&mut c, &park, "SetSpeedFraction", t![f64::NAN], d![])
                .is_err()
        );
        assert!(
            g.dispatch_object(
                &mut c,
                &park,
                "Orbit",
                t![g.state.c("STATION_ID"), -1],
                d![]
            )
            .is_err()
        );
        g.undock_ship(&mut other).unwrap();
        let other_park = bind(&mut g, &mut other);
        let other_id = other.id;
        g.clients.insert(other_id, other);
        let before = g.clients[&other_id].out.len();
        action(&mut g, &mut c, &park, "Stop", t![]);
        assert!(g.clients[&other_id].out.len() > before);
        // NPC health/removal is shared, and peer notifications are sent immediately.
        g.rat.hp = [0., 0., 1.];
        c.targets.insert(crate::destiny::RAT_ID);
        g.clients
            .get_mut(&other_id)
            .unwrap()
            .targets
            .insert(crate::destiny::RAT_ID);
        c.flight.as_mut().unwrap().position = [6000., 0., 10000.];
        let peer_out = g.clients[&other_id].out.len();
        let peer_deferred = g.clients[&other_id].deferred.len();
        g.rat_tick(&mut c, true).unwrap();
        assert!(!g.rat.alive());
        assert!(g.clients[&other_id].targets.is_empty());
        assert!(g.clients[&other_id].out.len() > peer_out);
        assert_eq!(g.clients[&other_id].deferred.len(), peer_deferred);
        g.rat.due = Some(Instant::now() - Duration::from_secs(1));
        g.tick().unwrap();
        assert!(g.rat.alive());
        // Snapshot has real ship metadata, peer ships, and a monotonically advancing stamp.
        c.objects.get_mut(&park).unwrap().epoch = Instant::now() - Duration::from_secs(5);
        c.deferred.clear();
        action(&mut g, &mut c, &park, "UpdateStateRequest", t![]);
        let payload = &c.deferred[0].body[0][1][1][1];
        let decoded = pickle::loads(payload.raw().unwrap()).unwrap();
        let update = &decoded[1][1][0][0];
        assert!(update[0].int() >= 11);
        let state = &update[1][1];
        assert_eq!(state[2], update[0]);
        assert_eq!(state[3], ship);
        assert!(
            state[1]
                .seq()
                .iter()
                .any(|slim| slim.state()["charID"] == g.clients[&other_id].char_id())
        );
        let (ball, slim) = crate::destiny::player_objects(&g.state, &c).unwrap();
        assert_eq!(slim.state()["ownerID"], c.char_id());
        assert_eq!(
            slim.state()["typeID"],
            g.state.ship_type(ship.clone()).unwrap()
        );
        assert!(matches!(ball, V::Reduce { .. }));
        // Persist on disconnect, then restore the same position and active ship.
        let saved = c.flight.as_ref().unwrap().position;
        let c_id = c.id;
        let account = c.account.clone();
        let character = c.char_id();
        g.clients.insert(c_id, c);
        g.disconnect(c_id);
        let mut c = Client::new(c_id);
        c.account = account;
        c.session = g.state.session_for(character).unwrap();
        assert!(c.session["stationid"].is_none());
        assert_eq!(c.ship(), ship);
        let park = bind(&mut g, &mut c);
        assert!(
            crate::space::length(crate::space::sub(
                c.flight.as_ref().unwrap().position,
                saved
            )) < 10.
        );
        // Dock close to the station, then reject every old ballpark request.
        let station = g.state.c("STATION_ID");
        let pos = crate::destiny::layout(&g.state, None).key(&station);
        c.flight.as_mut().unwrap().position = [
            pos[0].float(),
            pos[1].float(),
            pos[2].float() + pos[3].float() + 1000.,
        ];
        action(&mut g, &mut c, &park, "Dock", t![station.clone()]);
        assert_eq!(g.state.location(c.char_id()).unwrap()["stationID"], station);
        assert_eq!(
            g.state.item(ship.clone()).unwrap()["flag"],
            g.state.c("FLAG_HANGAR")
        );
        assert!(
            g.dispatch_object(&mut c, &park, "UpdateStateRequest", t![], d![])
                .is_err()
        );
        assert!(c.targets.is_empty());
        assert_eq!(g.state.item_damage(ship.clone()).unwrap(), 110.);
        let mut other = g.clients.remove(&other_id).unwrap();
        assert!(other.objects.contains_key(&other_park));
        g.dock_ship(&mut other, station).unwrap();
        g.state.migrate_gameplay().unwrap(); // repeated startup is nondestructive
        assert_eq!(g.state.balance(c.char_id()).unwrap(), 60.);
        assert_eq!(g.state.data["bulkdata.VERSION"], bulk);
    }
}
