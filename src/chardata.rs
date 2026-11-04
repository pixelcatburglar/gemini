use crate::{
    d, l,
    pickle::Value as V,
    state::{Error, Result, State, user},
    t,
    value::{row, rows},
};

impl State {
    pub fn data_row(&self, header: &str, line: Vec<V>) -> V {
        row(self.c(header), line)
    }
    pub fn table_row(&self, table: &str, header: &str, id: V) -> V {
        self.c(table)
            .seq()
            .iter()
            .find(|r| r[0] == id)
            .map(|r| {
                V::Dict(
                    self.c(header)
                        .seq()
                        .iter()
                        .cloned()
                        .zip(r.seq().iter().cloned())
                        .collect(),
                )
            })
            .unwrap_or(V::None)
    }
    pub fn type_info(&self, id: V) -> V {
        self.table_row("TYPES", "TYPE_HEADER", id)
    }
    pub fn group(&self, id: V) -> V {
        self.type_info(id)["groupID"].clone()
    }
    pub fn category(&self, id: V) -> V {
        self.table_row("GROUPS", "GROUP_HEADER", self.group(id))["categoryID"].clone()
    }
    pub fn pilot(&self, id: V) -> Result {
        let mut c = self.character(id)?;
        if c.is_none() {
            return Ok(c);
        }
        let bl = self.table_row("BLOODLINES", "BLOODLINE_HEADER", c["bloodlineID"].clone());
        for (key, fallback) in [
            ("security", 0.0.into()),
            ("raceID", bl["raceID"].clone()),
            (
                "typeID",
                (self.c("CHARACTER_TYPE_BASE").int() + c["bloodlineID"].int()).into(),
            ),
            ("corporationID", self.c("CORPORATION_ID")),
        ] {
            if c[key].is_none() {
                c.set(key, fallback);
            }
        }
        c.set("bloodlineName", bl["bloodlineName"].clone());
        Ok(c)
    }
    pub fn character_rows(&self, account: V) -> Result {
        let hierarchy = V::List(
            self.c("LOCATIONS")
                .seq()
                .iter()
                .map(|r| self.data_row("LOCATION_HEADER", r.seq().to_vec()))
                .collect(),
        );
        let mut out = vec![];
        for stored in self.query(
            "SELECT \"characterID\" FROM characters WHERE \"accountID\"=$1 ORDER BY \"characterID\"",
            &[account],
        )? {
            let c = self.pilot(stored["characterID"].clone())?;
            let mut line = [
                "characterID",
                "characterName",
                "bloodlineID",
                "raceID",
                "gender",
                "security",
                "corporationID",
                "typeID",
            ]
            .iter()
            .map(|k| c[*k].clone())
            .collect::<Vec<_>>();
            line.extend([hierarchy[3].clone(), hierarchy.clone()]);
            out.push(self.data_row("CHARACTER_HEADER", line));
        }
        Ok(V::List(out))
    }
    pub fn session_for(&self, id: V) -> Result {
        let c = self.pilot(id.clone())?;
        if c.is_none() {
            return Ok(d!["charid"=>id]);
        }
        let loc = self.location(id.clone())?;
        let station = if loc.is_none() {
            self.c("STATION_ID")
        } else {
            loc["stationID"].clone()
        };
        Ok(
            d!["charid"=>id,"corpid"=>c["corporationID"].clone(),"stationid"=>station.clone(),
            "locationid"=>station.or(loc["solarSystemID"].or(self.c("SOLAR_SYSTEM_ID"))),
            "solarsystemid"=>loc["solarSystemID"].or(self.c("SOLAR_SYSTEM_ID")),
            "constellationid"=>loc["constellationID"].or(self.c("CONSTELLATION_ID")),
            "regionid"=>loc["regionID"].or(self.c("REGION_ID")),"stationItem"=>self.template("station_item")],
        )
    }
    pub fn appearance(&self, id: V) -> Result {
        let lines=self.query("SELECT \"parameterName\",\"parameterValue\" FROM character_appearance WHERE \"characterID\"=$1 ORDER BY \"parameterName\"",&[id])?
            .iter().map(|r|vec![r["parameterName"].clone(),r["parameterValue"].clone()]).collect();
        Ok(rows(self.c("APPEARANCE_HEADER"), lines))
    }
    pub fn other_info(&self, id: V) -> Result {
        let c = self.pilot(id)?;
        if c.is_none() {
            return Ok(d![]);
        }
        let mut out = d!["securityStatus"=>c["security"].clone()];
        for k in [
            "characterID",
            "characterName",
            "bloodlineName",
            "gender",
            "raceID",
            "bloodlineID",
            "corporationID",
            "typeID",
        ] {
            out.set(k, c[k].clone());
        }
        Ok(out)
    }
    pub fn public_info(&self, id: V) -> Result {
        let id = if id.is_none() {
            self.one(
                "SELECT \"characterID\" FROM characters ORDER BY \"characterID\" LIMIT 1",
                &[],
            )?["characterID"]
                .clone()
        } else {
            id
        };
        let c = self.pilot(id.clone())?;
        let line = if c.is_none() {
            let mut line = vec![V::None; self.c("PUBLIC_INFO_HEADER").seq().len()];
            line[0] = id.clone();
            if id == self.c("CORPORATION_ID") {
                line[1] = self.c("CORPORATION_NAME");
                line[2] = self.p("typeCorporation");
                line[3] = id;
            }
            line
        } else {
            vec![
                c["characterID"].clone(),
                c["characterName"].clone(),
                c["typeID"].clone(),
                c["corporationID"].clone(),
                V::None,
                c["raceID"].clone(),
                c["bloodlineID"].clone(),
                c["gender"].clone(),
                c["typeID"].clone(),
                c["security"].clone(),
                0.0.into(),
                "".into(),
                "".into(),
                V::None,
                V::None,
            ]
        };
        Ok(self.data_row("PUBLIC_INFO_HEADER", line))
    }
    pub fn owners(&self) -> Result<Vec<V>> {
        let mut out = vec![
            l![self.c("CORPORATION_ID"), self.c("CORPORATION_NAME"), 2],
            l![self.c("PIRATE_OWNER_ID"), self.c("PIRATE_OWNER_NAME"), 2],
        ];
        for c in self.query("SELECT * FROM characters ORDER BY \"characterID\"", &[])? {
            out.push(l![
                c["characterID"].clone(),
                c["characterName"].clone(),
                c["typeID"].or((1372 + c["bloodlineID"].int()).into())
            ]);
        }
        Ok(out)
    }
    pub fn owner_name(&self, id: V) -> Result {
        Ok(self
            .owners()?
            .into_iter()
            .find(|r| r[0] == id)
            .map(|r| r[1].clone())
            .unwrap_or("".into()))
    }
    pub fn locations_ex(&self, ids: &[V]) -> Result {
        let known = self.c("LOCATIONS");
        let mut out = vec![];
        for id in ids {
            if let Some(location) = crate::reference::location(&self.db, id.int())? {
                out.push(location);
                continue;
            }
            if let Some(r) = known.seq().iter().find(|r| r[0] == *id) {
                out.push(r.clone());
                continue;
            }
            let it = self.item(id.clone())?;
            let name = if it.is_none() {
                format!("Unknown location {}", id.text())
            } else {
                let c = self.character(it["ownerID"].clone())?;
                let type_name = self.type_info(it["typeID"].clone())["typeName"]
                    .text()
                    .replace("Test ", "");
                if c.is_none() {
                    type_name
                } else {
                    format!("{}'s {type_name}", c["characterName"].text())
                }
            };
            out.push(l![
                id.clone(),
                name,
                0.0,
                0.0,
                0.0,
                self.c("SOLAR_SYSTEM_ID")
            ]);
        }
        Ok(t![self.c("LOCATION_HEADER"), V::List(out)])
    }
    pub fn owners_ex(&self, ids: &[V]) -> Result {
        let known = self.owners()?;
        Ok(t![
            self.c("OWNER_HEADER"),
            V::List(
                ids.iter()
                    .map(
                        |id| known.iter().find(|r| r[0] == *id).cloned().unwrap_or(l![
                            id.clone(),
                            format!("Unknown owner {}", id.text()),
                            0
                        ])
                    )
                    .collect()
            )
        ])
    }
    pub fn inventory_row(&self, it: &V) -> V {
        let line = vec![
            it["itemID"].clone(),
            it["typeID"].clone(),
            it["ownerID"].clone(),
            it["locationID"].clone(),
            it["flag"].clone(),
            0.into(),
            it["singleton"].clone(),
            it["quantity"].clone(),
            self.group(it["typeID"].clone()),
            self.category(it["typeID"].clone()),
            V::None,
            V::None,
        ];
        self.data_row("ITEM_HEADER", line)
    }
    pub fn gone_item(&self, id: V) -> V {
        let mut line = vec![V::None; self.c("ITEM_HEADER").seq().len()];
        line[0] = id;
        self.data_row("ITEM_HEADER", line)
    }
    pub fn hangar(&self, character: V) -> Result<Vec<V>> {
        self.query(
            "SELECT * FROM inventory WHERE \"locationID\"=$1 AND \"ownerID\"=$2 ORDER BY \"itemID\"",
            &[self.c("STATION_ID"), character],
        )
    }
    pub fn hangar_ids(&self, character: V) -> Result<Vec<V>> {
        Ok(self
            .hangar(character)?
            .iter()
            .filter(|r| r["flag"] == self.c("FLAG_HANGAR"))
            .map(|r| r["itemID"].clone())
            .collect())
    }
    pub fn cargo(&self, ship: V) -> Result<Vec<V>> {
        self.query(
            "SELECT * FROM inventory WHERE \"locationID\"=$1 AND flag=$2 ORDER BY \"itemID\"",
            &[ship, self.c("FLAG_CARGO")],
        )
    }
    pub fn cargo_used(&self, ship: V) -> Result<f64> {
        Ok((self
            .cargo(ship)?
            .iter()
            .map(|i| {
                self.type_info(i["typeID"].clone())["volume"].float()
                    * if i["quantity"].truth() {
                        i["quantity"].float()
                    } else {
                        1.
                    }
            })
            .sum::<f64>()
            * 10000.)
            .round()
            / 10000.)
    }
    pub fn ship_type(&self, ship: V) -> Result {
        Ok(self.item(ship)?["typeID"].or(self.c("SHIP_TYPE_ID")))
    }
    pub fn ship_item(&self, ship: V, location: V) -> Result {
        let it = self.item(ship.clone())?;
        Ok(self.inventory_row(&d!["itemID"=>ship,"typeID"=>it["typeID"].or(self.c("SHIP_TYPE_ID")),"ownerID"=>it["ownerID"].clone(),
            "locationID"=>location.or(self.c("SOLAR_SYSTEM_ID")),"flag"=>0,"singleton"=>1,"quantity"=>1]))
    }
    pub fn systems(&self, ship: V) -> Result<Vec<V>> {
        let type_id = self.ship_type(ship.clone())?;
        let spec = self.data["chardata._SHIPTYPE_BY_ID"].key(&type_id);
        let owner = self.item(ship.clone())?["ownerID"].clone();
        let mut out = vec![];
        for (index, slot) in self.c("SHIP_SYSTEM_SLOTS").seq().iter().enumerate() {
            let type_id = spec[&slot[1].text()[..]].clone();
            if type_id.is_none() {
                continue;
            }
            out.push(d!["itemID"=>(self.c("SYSTEM_ITEM_ID_BASE").int()+(ship.int()%1_000_000)*10+index as i64),
                "typeID"=>type_id.clone(),"ownerID"=>owner.clone(),"locationID"=>ship.clone(),"flag"=>self.c("FLAG_FIXED_SLOT"),"quantity"=>1,"singleton"=>1,
                "attributes"=>self.data["chardata._SYSTEM_ATTRIBUTES"].key(&type_id).or(d![])]);
        }
        Ok(out)
    }
    pub fn ship_attributes(&self, ship: V, location: V) -> Result {
        let mut out = d![ship.clone()=>self.data_row("SHIP_ATTRIBUTE_HEADER",vec![ship.clone(),self.ship_item(ship.clone(),location)?,self.data["catalog.SHIP_DOGMA_STATS"].key(&self.ship_type(ship.clone())?).or(d![])])];
        for s in self.systems(ship)? {
            out.set(
                s["itemID"].clone(),
                self.data_row(
                    "SHIP_ATTRIBUTE_HEADER",
                    vec![
                        s["itemID"].clone(),
                        self.inventory_row(&s),
                        s["attributes"].clone(),
                    ],
                ),
            );
        }
        Ok(out)
    }
    pub fn wallet_item(&self, owner: V) -> Result {
        let money = self.balance(owner.clone())?;
        if money == 0. {
            return Ok(V::None);
        }
        let mut value=self.inventory_row(&d!["itemID"=>(self.c("WALLET_ITEM_ID_BASE").int()+owner.int()%1_000_000),"typeID"=>self.p("typeCredits"),
            "ownerID"=>owner.clone(),"locationID"=>owner,"flag"=>self.p("flagWallet"),"singleton"=>0,"quantity"=>(money as i64)]);
        if let V::Instance { state, .. } = &mut value
            && let V::Dict(p) = state.as_mut()
            && let Some((_, V::List(line))) = p.iter_mut().find(|(k, _)| k.text() == "line")
        {
            line[8] = V::None;
            line[9] = V::None;
        }
        Ok(value)
    }
    pub fn corporation(&self) -> Result {
        let count = self.one("SELECT COUNT(*) AS n FROM characters", &[])?["n"].clone();
        let mut line = vec![
            self.c("CORPORATION_ID"),
            self.c("CORPORATION_NAME"),
            "".into(),
            self.c("CORPORATION_TICKER"),
            self.c("CORPORATION_ID"),
            self.c("CORPORATION_ID"),
            self.c("STATION_ID"),
            V::None,
            V::None,
            0.into(),
            count,
            0.0.into(),
            "".into(),
            self.c("NO_MODEL_GRAPHIC_ID"),
        ];
        line.extend(self.c("CORPORATION_LOGO").seq().to_vec());
        Ok(self.data_row("CORPORATION_HEADER", line))
    }
    pub fn members(&self, corp: V, character: V) -> Result {
        let mut lines = vec![];
        for stored in self.query(
            "SELECT \"characterID\" FROM characters ORDER BY \"characterID\"",
            &[],
        )? {
            if !character.is_none() && stored["characterID"] != character {
                continue;
            }
            let c = self.pilot(stored["characterID"].clone())?;
            if character.is_none() && !corp.is_none() && c["corporationID"] != corp {
                continue;
            }
            lines.push(vec![
                c["characterID"].clone(),
                c["corporationID"].clone(),
                c["characterName"].clone(),
                "".into(),
                0.into(),
                0.into(),
                V::None,
                V::None,
            ]);
        }
        Ok(rows(self.c("MEMBER_HEADER"), lines))
    }
    pub fn create_pilot(&mut self, account: V, args: &V) -> Result {
        if account.is_none() {
            return Err(user("No account on this session."));
        }
        if self.one(
            "SELECT COUNT(*) AS n FROM characters WHERE \"accountID\"=$1",
            std::slice::from_ref(&account),
        )?["n"]
            .int()
            >= 3
        {
            return Err(Error::User("NoFreeSlots".into(), d![]));
        }
        let name = args[0]
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let invalid = || Error::User("AskTryNewName".into(), d!["name"=>name.clone()]);
        if !(4..=16).contains(&name.chars().count())
            || name.split(' ').count() > 2
            || name
                .chars()
                .any(|c| c as u32 > 255 || (!c.is_alphabetic() && c != ' '))
        {
            return Err(invalid());
        }
        if self
            .query("SELECT \"characterName\" FROM characters", &[])?
            .iter()
            .any(|c| c["characterName"].text().trim().to_lowercase() == name.to_lowercase())
        {
            return Err(invalid());
        }
        let bl = self.table_row("BLOODLINES", "BLOODLINE_HEADER", args[1].clone());
        if bl.is_none() {
            return Err(invalid());
        }
        self.transaction(|s|{
            let id=V::from(s.next_id("characters","characterID",s.c("CHARACTER_ID_BASE").int())?);
            let ship=V::from(s.next_id("inventory","itemID",s.c("ITEM_ID_BASE").int())?);
            let mut c=d!["characterID"=>id.clone(),"accountID"=>account,"characterName"=>name,"bloodlineID"=>args[1].clone(),"raceID"=>bl["raceID"].clone(),"gender"=>args[2].clone(),"security"=>0.0,"corporationID"=>s.c("CORPORATION_ID"),"typeID"=>(1372+args[1].int())];
            for (i,col) in ["intelligence","charisma","perception","memory","willpower"].iter().enumerate(){c.set(*col,args[6+i].clone());}
            s.insert("characters",&c,false)?;
            s.insert("character_location",&d!["characterID"=>id.clone(),"stationID"=>s.c("STATION_ID"),"solarSystemID"=>s.c("SOLAR_SYSTEM_ID"),"constellationID"=>s.c("CONSTELLATION_ID"),"regionID"=>s.c("REGION_ID"),"shipID"=>V::None],false)?;
            s.insert("inventory",&d!["itemID"=>ship,"ownerID"=>id.clone(),"locationID"=>s.c("STATION_ID"),"typeID"=>s.c("SHIP_TYPE_ID"),"flag"=>s.c("FLAG_HANGAR"),"quantity"=>1,"singleton"=>1],false)?;
            s.money(id.clone(),s.c("STARTING_BALANCE").float()+args[3].float())?;
            for skill in args[4].seq().iter().chain(s.c("REAL_SKILL_TYPE_IDS").seq()) {
                if matches!(skill,V::Int(_)){s.exec("INSERT INTO skills(\"ownerID\",\"typeID\") VALUES($1,$2) ON CONFLICT DO NOTHING",&[id.clone(),skill.clone()])?;}
            }Ok(id)
        })
    }
    pub fn delete_pilot(&mut self, account: V, id: V) -> Result<bool> {
        if !self.owns(account, id.clone())? {
            return Ok(false);
        }
        self.transaction(|s| {
            for (table, key) in [
                ("character_appearance", "characterID"),
                ("character_location", "characterID"),
                ("skills", "ownerID"),
                ("market_orders", "charID"),
                ("market_transactions", "charID"),
                ("inventory", "ownerID"),
                ("wallet", "ownerID"),
                ("characters", "characterID"),
            ] {
                s.exec(
                    &format!("DELETE FROM {table} WHERE \"{key}\"=$1"),
                    std::slice::from_ref(&id),
                )?;
            }
            Ok(true)
        })
    }
}
