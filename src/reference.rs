//! PostgreSQL reference data. Protocol object descriptions are tagged JSON; game
//! tables are ordinary SQL columns. Descriptions link to rows instead of
//! keeping separate, stale copies of the catalog in client response templates.
use crate::db::Connection;
use crate::{
    d, l,
    pickle::Value as V,
    state::{Error, Result},
    value::row,
};
use serde_json::Value as Json;
use std::collections::BTreeMap;

pub fn identifier(name: &str) -> Result<String> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(format!("invalid reference SQL identifier: {name}").into());
    }
    Ok(format!("\"{name}\""))
}
fn json_error(e: serde_json::Error) -> Error {
    e.to_string().into()
}
pub fn encode(value: &V) -> String {
    serde_json::to_string(value).expect("serializable protocol value")
}
pub fn decode(text: &str) -> Result<V> {
    serde_json::from_str(text).map_err(json_error)
}

pub struct Table {
    pub header: V,
    pub rows: Vec<V>,
    pub ordinals: Vec<i64>,
}
impl Table {
    fn cells(value: &V) -> Vec<V> {
        if matches!(value, V::Dict(_)) {
            value.pairs().iter().map(|(_, v)| v.clone()).collect()
        } else {
            value.seq().to_vec()
        }
    }
    pub fn render(&self, view: &str, ordinal: Option<i64>) -> Result<V> {
        let make = |r: &V, shape: &str| -> Result<V> {
            let cells = Self::cells(r);
            Ok(match shape {
                "row" => r.clone(),
                "list" => V::List(cells),
                "tuple" => V::Tuple(cells),
                "dict" => V::Dict(self.header.seq().iter().cloned().zip(cells).collect()),
                "object" => row(self.header.clone(), cells),
                _ => return Err(format!("unknown row view {shape}").into()),
            })
        };
        if let Some(id) = ordinal {
            let index = self
                .ordinals
                .iter()
                .position(|v| *v == id)
                .ok_or("reference row missing")?;
            return make(&self.rows[index], view);
        }
        Ok(match view {
            "header" => self.header.clone(),
            "rows" => V::List(self.rows.clone()),
            "lists" | "tuples" | "dicts" | "objects" => V::List(
                self.rows
                    .iter()
                    .map(|r| make(r, view.trim_end_matches('s')))
                    .collect::<Result<Vec<_>>>()?,
            ),
            "indexed_objects" | "keyed_tuples" | "keyed_lists" | "keyed_dicts" => {
                let mut pairs = Vec::new();
                for r in &self.rows {
                    let cells = Self::cells(r);
                    let key = cells.first().ok_or("empty keyed row")?.clone();
                    let value = match view {
                        "indexed_objects" => make(r, "object")?,
                        "keyed_tuples" => V::Tuple(cells[1..].to_vec()),
                        "keyed_lists" => V::List(cells[1..].to_vec()),
                        _ => V::Dict(
                            self.header.seq()[1..]
                                .iter()
                                .cloned()
                                .zip(cells[1..].to_vec())
                                .collect(),
                        ),
                    };
                    pairs.push((key, value));
                }
                V::Dict(pairs)
            }
            _ => return Err(format!("unknown table view {view}").into()),
        })
    }
}

pub fn tables(db: &Connection) -> Result<BTreeMap<String, Table>> {
    let mut out = BTreeMap::new();
    for entry in db.query(
        "SELECT table_name,header FROM reference_tables ORDER BY table_name",
        &[],
    )? {
        let name = entry["table_name"].text();
        let header = decode(&entry["header"].text())?;
        let columns = header
            .seq()
            .iter()
            .map(|v| identifier(&v.text()))
            .collect::<Result<Vec<_>>>()?;
        let mut rows = Vec::new();
        let mut ordinals = Vec::new();
        for r in db.query(
            &format!(
                "SELECT _ordinal,_shape,_kinds,{} FROM {} ORDER BY _ordinal",
                columns.join(","),
                identifier(&name)?
            ),
            &[],
        )? {
            ordinals.push(r["_ordinal"].int());
            let kinds: Vec<String> =
                serde_json::from_str(&r["_kinds"].text()).map_err(json_error)?;
            if kinds.len() != columns.len() {
                return Err(format!("invalid cell metadata in {name}").into());
            }
            let mut cells = Vec::new();
            for (column, kind) in header.seq().iter().zip(kinds) {
                let value = r.key(column);
                cells.push(if value.is_none() {
                    V::None
                } else {
                    match kind.as_str() {
                        "int" => {
                            if matches!(value, V::Float(_)) {
                                value.int().into()
                            } else {
                                V::Int(value.text())
                            }
                        }
                        "float" => value
                            .text()
                            .parse::<f64>()
                            .map_err(|e| e.to_string())?
                            .into(),
                        "json" => decode(&value.text())?,
                        "unicode" => V::Unicode(value.text()),
                        "bytes" => value.text().into(),
                        _ => value,
                    }
                });
            }
            rows.push(match r["_shape"].text().as_str() {
                "dict" => V::Dict(header.seq().iter().cloned().zip(cells).collect()),
                "tuple" => V::Tuple(cells),
                "list" => V::List(cells),
                _ => return Err(format!("invalid row shape in {name}").into()),
            });
        }
        out.insert(
            name,
            Table {
                header,
                rows,
                ordinals,
            },
        );
    }
    Ok(out)
}
fn resolve(json: &mut Json, tables: &BTreeMap<String, Table>, depth: usize) -> Result<()> {
    if depth > 128 {
        return Err("reference document too deep".into());
    }
    if let Some(name) = json.get("$table").and_then(Json::as_str) {
        let table = tables
            .get(name)
            .ok_or_else(|| format!("missing reference table {name}"))?;
        let view = json
            .get("view")
            .and_then(Json::as_str)
            .ok_or("missing table view")?;
        *json = serde_json::to_value(table.render(view, json.get("row").and_then(Json::as_i64))?)
            .map_err(json_error)?;
    } else {
        match json {
            Json::Array(values) => {
                for value in values {
                    resolve(value, tables, depth + 1)?;
                }
            }
            Json::Object(values) => {
                for value in values.values_mut() {
                    resolve(value, tables, depth + 1)?;
                }
            }
            _ => (),
        }
    }
    Ok(())
}
pub fn load(db: &Connection) -> Result<V> {
    if db.one(
        "SELECT value FROM meta WHERE key='server_reference_version'",
        &[],
    )?["value"]
        .text()
        != "1"
    {
        return Err("unsupported reference schema version".into());
    }
    validate_catalog(db)?;
    let tables = tables(db)?;
    let mut data = d![];
    for entry in db.query(
        "SELECT name,document FROM reference_documents ORDER BY ordinal",
        &[],
    )? {
        let mut json = serde_json::from_str(&entry["document"].text()).map_err(json_error)?;
        resolve(&mut json, &tables, 0)?;
        data.set(
            entry["name"].clone(),
            serde_json::from_value::<V>(json).map_err(json_error)?,
        );
    }
    for scope in ["services", "objects", "any"] {
        data.set(format!("rpc.{scope}"), d![]);
    }
    for r in &tables
        .get("game_rpc_methods")
        .ok_or("RPC table missing")?
        .rows
    {
        let key = format!("rpc.{}", r[0].text());
        let mut scope = data[&key[..]].clone();
        if r[0].text() == "any" {
            scope.set(r[2].clone(), r[3].clone());
        } else {
            let mut target = scope.key(&r[1]).or(d![]);
            target.set(r[2].clone(), r[3].clone());
            scope.set(r[1].clone(), target);
        }
        data.set(key, scope);
    }
    for r in &tables
        .get("game_dogma_values")
        .ok_or("dogma table missing")?
        .rows
    {
        let mut values = data.key(&r[0]).or(d![]);
        let mut owner = values.key(&r[1]).or(d![]);
        owner.set(r[2].clone(), r[3].clone());
        values.set(r[1].clone(), owner);
        data.set(r[0].clone(), values);
    }
    let version = db.one("SELECT value FROM meta WHERE key='bulk_version'", &[])?["value"].text();
    let version: i64 = version.parse().map_err(|_| "invalid bulk version")?;
    data.set("bulkdata.VERSION", version);
    let mut auth = data["template.auth"].clone();
    if let V::Dict(pairs) = &mut auth {
        for (_, object) in pairs {
            if let V::Instance { state, .. } = object
                && let V::Tuple(values) = state.as_mut()
                && values.len() == 4
            {
                values[3] = version.into();
            }
        }
    }
    data.set("template.auth", auth);
    for required in [
        "chardata.TYPES",
        "chardata.BLOODLINES",
        "bulkdata.OBJECTS",
        "template.seed",
        "template.prices",
        "rpc.services",
        "rpc.objects",
    ] {
        if data[required].is_none() {
            return Err(format!("missing reference dataset {required}").into());
        }
    }
    refresh_universe(db, &mut data)?;
    Ok(data)
}

/// Names and coordinates of named universe objects always come from the map DB.
pub fn location(db: &Connection, id: i64) -> Result<Option<V>> {
    for (table, key, parent) in [
        ("regions", "regionID", "NULL"),
        ("constellations", "constellationID", "NULL"),
        ("solar_systems", "solarSystemID", "\"solarSystemID\""),
    ] {
        let result = db.one(
            &format!(
                "SELECT {} AS id,name,x,y,z,{parent} AS parent FROM {table} WHERE {}=$1",
                identifier(key)?,
                identifier(key)?
            ),
            &[id.into()],
        )?;
        if !result.is_none() {
            return Ok(Some(l![
                result["id"].clone(),
                result["name"].clone(),
                result["x"].clone(),
                result["y"].clone(),
                result["z"].clone(),
                result["parent"].clone()
            ]));
        }
    }
    Ok(None)
}
fn refresh_universe(db: &Connection, data: &mut V) -> Result<()> {
    let system = data["chardata.SOLAR_SYSTEM_ID"].int();
    let home = db.one(
        "SELECT \"constellationID\",\"regionID\" FROM solar_systems WHERE \"solarSystemID\"=$1",
        &[system.into()],
    )?;
    let constellation = home["constellationID"].int();
    let region = home["regionID"].int();
    if constellation != data["chardata.CONSTELLATION_ID"].int()
        || region != data["chardata.REGION_ID"].int()
    {
        return Err("configured home-system hierarchy disagrees with map database".into());
    }
    let mut locations = vec![];
    for id in [region, constellation, system] {
        locations.push(location(db, id)?.ok_or("home map location missing")?);
    }
    let station = data["chardata.LOCATIONS"][3].clone();
    let mut station_cells = station.seq().to_vec();
    if station_cells.len() != 6 {
        return Err("home station location missing".into());
    }
    station_cells[2..5].clone_from_slice(&locations[2].seq()[2..5]);
    locations.push(V::List(station_cells));
    data.set("chardata.LOCATIONS", V::List(locations.clone()));
    for (i, key) in ["_REGION", "_CONSTELLATION", "_SYSTEM", "_STATION"]
        .iter()
        .enumerate()
    {
        data.set(format!("chardata.{key}"), locations[i].clone());
    }
    let mut objects = data["bulkdata.OBJECTS"].clone();
    objects.set(
        "config.BulkData.locations",
        crate::t![
            data["chardata.LOCATION_HEADER"].clone(),
            V::List(locations.clone())
        ],
    );
    data.set("bulkdata.OBJECTS", objects);
    let hierarchy = [2, 1, 0]
        .iter()
        .map(|&i| {
            let mut line = locations[i].seq().to_vec();
            line.push(
                data["template.map_hierarchy"].state()["line"][0][2 - i].state()["line"][6].clone(),
            );
            row(data["chardata.HIERARCHY_HEADER"].clone(), line)
        })
        .collect();
    data.set(
        "template.map_hierarchy",
        row(l!["hierarchy"], vec![V::List(hierarchy)]),
    );
    Ok(())
}

pub fn check(db: &Connection) -> Result<()> {
    validate_catalog(db)?;
    if db.one("SELECT COUNT(*) AS n FROM pg_constraint WHERE connamespace=current_schema()::regnamespace AND NOT convalidated", &[])?["n"].int()!=0 {
        return Err("database has unvalidated constraints".into());
    }
    Ok(())
}
fn validate_catalog(db: &Connection) -> Result<()> {
    for table in [
        "regions",
        "constellations",
        "solar_systems",
        "system_contents",
        "game_types",
        "game_groups",
        "game_categories",
        "game_graphics",
        "game_messages",
        "game_market_prices",
        "game_bloodlines",
        "game_races",
        "game_rpc_methods",
    ] {
        let count = db.one(&format!("SELECT COUNT(*) AS n FROM {table}"), &[])?["n"].int();
        if count == 0 {
            return Err(format!("reference table {table} is empty").into());
        }
    }
    for (name, sql) in [
        (
            "type groups",
            "SELECT COUNT(*) AS n FROM game_types t LEFT JOIN game_groups g ON t.\"groupID\"=g.\"groupID\" WHERE g.\"groupID\" IS NULL",
        ),
        (
            "group categories",
            "SELECT COUNT(*) AS n FROM game_groups g LEFT JOIN game_categories c ON g.\"categoryID\"=c.\"categoryID\" WHERE c.\"categoryID\" IS NULL",
        ),
        (
            "type graphics",
            "SELECT COUNT(*) AS n FROM game_types t LEFT JOIN game_graphics g ON t.\"graphicID\"=g.\"graphicID\" WHERE t.\"graphicID\" IS NOT NULL AND g.\"graphicID\" IS NULL",
        ),
        (
            "market price types",
            "SELECT COUNT(*) AS n FROM game_market_prices p LEFT JOIN game_types t ON p.\"typeID\"=t.\"typeID\" WHERE t.\"typeID\" IS NULL",
        ),
        (
            "missing market prices",
            "SELECT COUNT(*) AS n FROM game_types t LEFT JOIN game_market_prices p ON p.\"typeID\"=t.\"typeID\" WHERE p.\"typeID\" IS NULL",
        ),
        (
            "bloodline races",
            "SELECT COUNT(*) AS n FROM game_bloodlines b LEFT JOIN game_races r ON b.\"raceID\"=r.\"raceID\" WHERE r.\"raceID\" IS NULL",
        ),
    ] {
        let count = db.one(sql, &[])?["n"].int();
        if count != 0 {
            return Err(format!("invalid reference data: {name} ({count} rows)").into());
        }
    }
    Ok(())
}
