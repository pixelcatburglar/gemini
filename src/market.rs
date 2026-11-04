use crate::{
    d, l,
    pickle::Value as V,
    state::{Error, Result, State, now},
    t,
    value::row,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Debug)]
pub struct Changes {
    pub filled: i64,
    pub spent: f64,
    pub earned: f64,
    pub order: Option<i64>,
    pub added: Vec<V>,
    pub changed: Vec<V>,
    pub removed: Vec<V>,
    pub others: BTreeMap<i64, (Vec<V>, Vec<V>)>,
}
pub struct Order {
    pub owner: V,
    pub type_id: V,
    pub sub: V,
    pub quantity: i64,
    pub price: i64,
    pub range: V,
    pub duration: V,
    pub bid: bool,
    pub item: V,
    pub station: V,
}
fn refusal(text: &str) -> Error {
    Error::User("CustomError".into(), d!["error"=>text])
}
pub fn type_info(value: &V) -> Result<(V, V)> {
    if matches!(value, V::Int(_)) {
        return Ok((value.clone(), 0.into()));
    }
    let state = value.state();
    let bag = state["bag"].or(state.clone());
    if bag["typeID"].is_none() {
        return Err(refusal("That item cannot be traded."));
    }
    Ok((bag["typeID"].clone(), bag["subTypeID"].or(0.into())))
}
pub fn order_type(type_id: V, sub: V) -> V {
    V::instance(
        "OrderTypeInfo",
        d!["bag"=>d!["typeID"=>type_id,"subTypeID"=>sub]],
    )
}
fn blue_time(time: f64) -> V {
    (((time + 11644473600.) * 10_000_000.) as i64).into()
}
impl State {
    fn market_header(&self, name: &str) -> V {
        self.data[&format!("market.{name}")[..]].clone()
    }
    pub fn home(&self) -> V {
        d!["stationID"=>self.c("STATION_ID"),"solarSystemID"=>self.c("SOLAR_SYSTEM_ID"),"constellationID"=>self.c("CONSTELLATION_ID"),"regionID"=>self.c("REGION_ID")]
    }
    pub fn types_under(&self, group: V) -> Vec<V> {
        let mut pending = vec![group];
        let mut seen = BTreeSet::new();
        let mut out = BTreeSet::new();
        while let Some(g) = pending.pop() {
            if !seen.insert(g.int()) {
                continue;
            }
            for typ in self.c("TYPES").seq() {
                if typ[6] == g {
                    out.insert(typ[0].int());
                }
            }
            for entry in self.data["catalog.MARKET_TREE"].seq() {
                if entry[1] == g {
                    pending.push(entry[0].clone());
                }
            }
        }
        out.into_iter().map(V::from).collect()
    }
    pub fn price(&self, typ: &V) -> i64 {
        self.template("prices").key(typ)[0].or(10000.into()).int()
    }
    fn stock(&self, typ: &V) -> i64 {
        self.template("prices").key(typ)[1].or(100.into()).int()
    }
    pub fn visible(&self, order: &V, viewer: &V, range: &V) -> bool {
        let distance = ["stationID", "solarSystemID", "constellationID", "regionID"]
            .iter()
            .position(|k| order[*k] == viewer[*k])
            .unwrap_or(4) as i64;
        let ranks = &self.data["market._RANGE_RANK"];
        distance <= ranks.key(range).or(3.into()).int()
            && distance <= ranks.key(&order["orderRange"]).or(0.into()).int()
    }
    fn stand(&self, o: &Order, quantity: i64, days: i64, escrow: V, time: f64) -> Result<i64> {
        let id = self.next_id("market_orders", "orderID", 95000001)?;
        let mut record = self.home();
        for (k,v) in d!["orderID"=>id,"typeID"=>o.type_id.clone(),"subTypeID"=>o.sub.clone(),"charID"=>o.owner.clone(),"bid"=>i32::from(o.bid),"price"=>(o.price as f64),"volEntered"=>quantity,"volRemaining"=>quantity,"minVolume"=>1,"orderRange"=>o.range.clone(),"duration"=>days,"issued"=>time,"escrowItemID"=>escrow].pairs(){record.set(k.clone(),v.clone());}
        self.insert("market_orders", &record, false)?;
        Ok(id)
    }
    pub fn boot_market(&mut self) -> Result<()> {
        self.transaction(|s|{
            let time=now();
            for o in s.query("SELECT * FROM market_orders WHERE \"charID\" IS NOT NULL",&[])? {
                if time-o["issued"].float()>=o["duration"].float()*86400. {s.refund(&o)?;s.exec("DELETE FROM market_orders WHERE \"orderID\"=$1",&[o["orderID"].clone()])?;}
            }
            for typ in s.c("TYPES").seq(){
                if typ[6].is_none(){continue;}
                for bid in [false,true] {
                    if !s.one("SELECT \"orderID\" FROM market_orders WHERE \"charID\" IS NULL AND \"typeID\"=$1 AND bid=$2",&[typ[0].clone(),i32::from(bid).into()])?.is_none(){continue;}
                    let price=if bid {((s.price(&typ[0]) as f64*0.70).round_ties_even() as i64).max(1)}else{s.price(&typ[0])};
                    s.stand(&Order{owner:V::None,type_id:typ[0].clone(),sub:0.into(),quantity:0,price,range:s.p("rangeRegion"),duration:365.into(),bid,item:V::None,station:V::None},s.stock(&typ[0]),365,V::None,time)?;
                }
            }Ok(())
        })
    }
    fn book_orders(&self, typ: V, sub: V, viewer: &V, range: &V) -> Result<Vec<V>> {
        let time = now();
        let target = self.stock(&typ);
        for o in self.query(
            "SELECT * FROM market_orders WHERE \"typeID\"=$1 AND \"charID\" IS NULL",
            std::slice::from_ref(&typ),
        )? {
            if time - o["issued"].float() < 3600. {
                continue;
            }
            if o["volRemaining"].int() < target {
                self.exec(
                    "UPDATE market_orders SET \"volRemaining\"=$1,\"volEntered\"=$2,issued=$3 WHERE \"orderID\"=$4",
                    &[
                        target.into(),
                        target.into(),
                        time.into(),
                        o["orderID"].clone(),
                    ],
                )?;
            } else {
                self.exec(
                    "UPDATE market_orders SET issued=$1 WHERE \"orderID\"=$2",
                    &[time.into(), o["orderID"].clone()],
                )?;
            }
        }
        Ok(self
            .query(
                "SELECT * FROM market_orders WHERE \"typeID\"=$1 AND \"subTypeID\"=$2 ORDER BY \"orderID\"",
                &[typ, sub],
            )?
            .into_iter()
            .filter(|o| self.visible(o, viewer, range))
            .collect())
    }
    pub fn book(&self, typ: V, sub: V, viewer: &V, range: &V) -> Result {
        let orders = self.book_orders(typ, sub, viewer, range)?;
        let mut sides = vec![];
        for bid in [false, true] {
            let mut side = orders
                .iter()
                .filter(|o| o["bid"].truth() == bid)
                .collect::<Vec<_>>();
            side.sort_by_key(|o| {
                (
                    if bid {
                        -o["price"].int()
                    } else {
                        o["price"].int()
                    },
                    o["orderID"].int(),
                )
            });
            let out = side
                .into_iter()
                .map(|o| {
                    let mut fields = vec![
                        o["volRemaining"].clone(),
                        o["price"].int().into(),
                        o["stationID"].clone(),
                        o["orderRange"].clone(),
                    ];
                    if !bid {
                        fields.push(o["escrowItemID"].or(o["orderID"].clone()));
                    }
                    V::Tuple(fields)
                })
                .collect();
            sides.push(V::List(out));
        }
        Ok(d!["asks"=>sides[0].clone(),"bids"=>sides[1].clone()])
    }
    fn candles(&self, typ: V, days: i64) -> Result<Vec<V>> {
        let today = (now() / 86400.).floor() as i64;
        let mut out = vec![];
        for day in today - days..today {
            let real = self.one(
                "SELECT * FROM market_price_history WHERE \"regionID\"=$1 AND \"typeID\"=$2 AND day=$3",
                &[self.c("REGION_ID"), typ.clone(), day.into()],
            )?;
            if !real.is_none() {
                out.push(real);
                continue;
            }
            let digest = crc32fast::hash(format!("{}@{day}", typ.int()).as_bytes());
            let swing = ((digest % 2001) as f64 / 1000. - 1.) * 0.12;
            let close = (self.price(&typ) as f64 * (1. + swing))
                .round_ties_even()
                .max(1.);
            let spread = (close * 0.04).round_ties_even().max(1.);
            out.push(d!["day"=>day,"lowPrice"=>(close-spread),"highPrice"=>(close+spread),"closePrice"=>close,"volume"=>(((digest>>8)%400) as i64),"orders"=>(((digest>>20)%12) as i64)]);
        }
        Ok(out)
    }
    pub fn history(&self, typ: V) -> Result {
        let mut out = vec![];
        for c in self.candles(typ, 30)?.iter().rev() {
            let when = blue_time(c["day"].float() * 86400. + 43200.);
            out.push(row(
                self.market_header("HISTORY_HEADER"),
                vec![
                    when.clone(),
                    c["lowPrice"].int().into(),
                    c["highPrice"].int().into(),
                    c["closePrice"].int().into(),
                    c["volume"].clone(),
                    c["orders"].clone(),
                    when,
                ],
            ));
        }
        Ok(V::List(out))
    }
    pub fn quote(&self, typ: V, sub: V, viewer: &V, range: &V) -> Result {
        let orders = self.book_orders(typ.clone(), sub, viewer, range)?;
        let candles = self.candles(typ.clone(), 2)?;
        let c = &candles[1];
        let ask = orders
            .iter()
            .filter(|o| !o["bid"].truth())
            .map(|o| o["price"].int())
            .min()
            .unwrap_or(0);
        let bid = orders
            .iter()
            .filter(|o| o["bid"].truth())
            .map(|o| o["price"].int())
            .max()
            .unwrap_or(0);
        Ok(row(
            self.market_header("QUOTE_HEADER"),
            vec![
                typ,
                c["highPrice"].int().into(),
                bid.into(),
                ask.into(),
                c["lowPrice"].int().into(),
                c["closePrice"].int().into(),
                c["volume"].clone(),
                ((c["closePrice"].float() - candles[0]["closePrice"].float()) as i64).into(),
            ],
        ))
    }
    pub fn my_orders(&self, owner: V, viewer: &V, range: &V) -> Result {
        let orders = self.query(
            "SELECT * FROM market_orders WHERE \"charID\"=$1 ORDER BY bid,\"typeID\",\"orderID\"",
            &[owner],
        )?;
        let mut out = vec![];
        for o in orders {
            if !self.visible(&o, viewer, range) {
                continue;
            }
            out.push(row(
                self.market_header("MY_ORDER_HEADER"),
                vec![
                    o["orderID"].clone(),
                    order_type(o["typeID"].clone(), o["subTypeID"].clone()),
                    o["charID"].clone(),
                    o["stationID"].clone(),
                    o["price"].int().into(),
                    o["volEntered"].clone(),
                    o["volRemaining"].clone(),
                    blue_time(o["issued"].float()),
                    blue_time(o["issued"].float() + o["duration"].float() * 86400.),
                    o["bid"].clone(),
                    o["orderRange"].clone(),
                    o["minVolume"].clone(),
                    o["duration"].clone(),
                    o["escrowItemID"].clone(),
                ],
            ));
        }
        Ok(t![l![], V::List(out)])
    }
    pub fn find_stack(&self, owner: V, location: V, typ: V, flag: V) -> Result {
        self.one("SELECT * FROM inventory WHERE \"ownerID\"=$1 AND \"locationID\"=$2 AND \"typeID\"=$3 AND flag=$4 AND singleton=0 ORDER BY \"itemID\" LIMIT 1",&[owner,location,typ,flag])
    }
    fn deposit(&self, owner: V, typ: V, quantity: i64) -> Result<(Vec<V>, Vec<V>)> {
        if quantity <= 0 {
            return Ok((vec![], vec![]));
        }
        let single = self.template("prices").key(&typ)[2].or(1.into()).truth();
        if !single {
            let stack = self.find_stack(
                owner.clone(),
                self.c("STATION_ID"),
                typ.clone(),
                self.c("FLAG_HANGAR"),
            )?;
            if !stack.is_none() {
                self.quantity(stack["itemID"].clone(), stack["quantity"].int() + quantity)?;
                return Ok((vec![], vec![stack["itemID"].clone()]));
            }
        }
        let mut added = vec![];
        for _ in 0..if single { quantity } else { 1 } {
            let id = V::from(self.next_id("inventory", "itemID", self.c("ITEM_ID_BASE").int())?);
            self.insert("inventory",&d!["itemID"=>id.clone(),"ownerID"=>owner.clone(),"locationID"=>self.c("STATION_ID"),"typeID"=>typ.clone(),"flag"=>self.c("FLAG_HANGAR"),"quantity"=>if single{1}else{quantity},"singleton"=>i32::from(single)],false)?;
            added.push(id);
        }
        Ok((added, vec![]))
    }
    fn refund(&self, o: &V) -> Result<(Vec<V>, Vec<V>)> {
        if o["bid"].truth() {
            self.money(
                o["charID"].clone(),
                o["price"].float() * o["volRemaining"].float(),
            )?;
            return Ok((vec![], vec![]));
        }
        let item = self.item(o["escrowItemID"].clone())?;
        if item.is_none() {
            return Ok((vec![], vec![]));
        }
        if !item["singleton"].truth() {
            let stack = self.find_stack(
                item["ownerID"].clone(),
                item["locationID"].clone(),
                item["typeID"].clone(),
                self.c("FLAG_HANGAR"),
            )?;
            if !stack.is_none() {
                self.quantity(
                    stack["itemID"].clone(),
                    stack["quantity"].int() + item["quantity"].int(),
                )?;
                self.quantity(item["itemID"].clone(), 0)?;
                return Ok((vec![stack["itemID"].clone()], vec![item["itemID"].clone()]));
            }
        }
        self.exec(
            "UPDATE inventory SET flag=$1 WHERE \"itemID\"=$2",
            &[self.c("FLAG_HANGAR"), item["itemID"].clone()],
        )?;
        Ok((vec![item["itemID"].clone()], vec![]))
    }
    pub fn cancel_order(&mut self, owner: V, id: V) -> Result<Changes> {
        self.transaction(|s| {
            let o = s.one(
                "SELECT * FROM market_orders WHERE \"orderID\"=$1",
                std::slice::from_ref(&id),
            )?;
            if o.is_none() {
                return Err(refusal("That order is no longer open."));
            }
            if o["charID"] != owner {
                return Err(refusal("That is not your order."));
            }
            let (changed, removed) = s.refund(&o)?;
            s.exec("DELETE FROM market_orders WHERE \"orderID\"=$1", &[id])?;
            Ok(Changes {
                changed,
                removed,
                earned: if o["bid"].truth() {
                    o["price"].float() * o["volRemaining"].float()
                } else {
                    0.
                },
                ..Changes::default()
            })
        })
    }
    fn record_trade(
        &self,
        parties: (V, V),
        typ: V,
        qty: i64,
        price: i64,
        time: f64,
        buyer_escrow: bool,
    ) -> Result<()> {
        let (buyer, seller) = parties;
        if !buyer.is_none() && !buyer_escrow {
            self.money(buyer.clone(), -(price as f64) * (qty as f64))?;
        }
        if !seller.is_none() {
            self.money(seller.clone(), (price as f64) * (qty as f64))?;
        }
        for (who, other, bid) in [(buyer.clone(), seller.clone(), 1), (seller, buyer, 0)] {
            if who.is_none() {
                continue;
            }
            self.exec("INSERT INTO market_transactions(\"charID\",\"clientID\",\"typeID\",quantity,price,bid,\"stationID\",\"regionID\",\"transactionDate\") VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",&[who,other,typ.clone(),qty.into(),price.into(),bid.into(),self.c("STATION_ID"),self.c("REGION_ID"),time.into()])?;
        }
        self.exec("INSERT INTO market_price_history(\"regionID\",\"typeID\",day,\"lowPrice\",\"highPrice\",\"closePrice\",volume,orders) VALUES($1,$2,$3,$4,$5,$6,$7,1) ON CONFLICT(\"regionID\",\"typeID\",day) DO UPDATE SET \"lowPrice\"=LEAST(market_price_history.\"lowPrice\",excluded.\"lowPrice\"),\"highPrice\"=GREATEST(market_price_history.\"highPrice\",excluded.\"highPrice\"),\"closePrice\"=excluded.\"closePrice\",volume=market_price_history.volume+excluded.volume,orders=market_price_history.orders+1",&[self.c("REGION_ID"),typ,((time/86400.).floor() as i64).into(),price.into(),price.into(),price.into(),qty.into()])?;
        Ok(())
    }
    pub fn place_order(&mut self, mut order: Order) -> Result<Changes> {
        if order.owner.is_none() {
            return Err(refusal("No character selected."));
        }
        if order.quantity <= 0 {
            return Err(refusal("Enter a quantity of at least one."));
        }
        if order.price <= 0 {
            return Err(refusal("Enter a price of at least one ISK."));
        }
        if self.type_info(order.type_id.clone())["marketGroupID"].is_none() {
            return Err(refusal("That item cannot be traded."));
        }
        let days = if !order.duration.truth() {
            0
        } else {
            self.data["market._DURATION_CONSTANTS"]
                .key(&order.duration)
                .or(order.duration.clone())
                .int()
                .clamp(1, 365)
        };
        // The native implementation makes a whole placement atomic.
        self.transaction(|s|{
            let mut result=Changes::default();let time=now();let mut remaining=order.quantity;
            let mut source=V::None;
            if !order.bid {
                source=if !order.item.is_none(){s.item(order.item.clone())?}else{s.hangar(order.owner.clone())?.into_iter().find(|r|r["typeID"]==order.type_id && r["flag"]!=s.p("flagMarket") && r["quantity"].int()>=remaining).unwrap_or(V::None)};
                if source.is_none() || source["ownerID"]!=order.owner || source["typeID"]!=order.type_id || source["flag"]==s.p("flagMarket") || source["locationID"]!=s.c("STATION_ID") || source["quantity"].int()<remaining {
                    return Err(refusal("You do not own that quantity of this item in the station hangar."));
                }
            }
            let mut matches=s.book_orders(order.type_id.clone(),order.sub.clone(),&s.home(),&order.range)?.into_iter().filter(|o|o["bid"].truth()!=order.bid && (o["charID"].is_none() || o["charID"]!=order.owner) && (order.station.is_none() || o["stationID"]==order.station)).collect::<Vec<_>>();
            matches.sort_by_key(|o|(if order.bid{o["price"].int()}else{-o["price"].int()},o["orderID"].int()));
            let mut broke=false;
            for o in matches {
                let unit=o["price"].int();if remaining<=0 || (order.bid && unit>order.price)||(!order.bid && unit<order.price){break;}
                let mut take=remaining.min(o["volRemaining"].int());
                if order.bid {take=take.min((s.balance(order.owner.clone())?/unit as f64).floor() as i64);if take<=0{broke=true;break;}
                    if !o["escrowItemID"].is_none(){let item=s.item(o["escrowItemID"].clone())?;if item.is_none(){s.exec("DELETE FROM market_orders WHERE \"orderID\"=$1",&[o["orderID"].clone()])?;continue;}
                        s.quantity(item["itemID"].clone(),item["quantity"].int()-take)?;}
                    s.record_trade((order.owner.clone(),o["charID"].clone()),order.type_id.clone(),take,unit,time,false)?;
                    let (added,changed)=s.deposit(order.owner.clone(),order.type_id.clone(),take)?;result.added.extend(added);result.changed.extend(changed);result.spent+=unit as f64*take as f64;
                    if !o["charID"].is_none(){result.others.entry(o["charID"].int()).or_default();}
                } else {
                    let left=source["quantity"].int()-take;s.quantity(source["itemID"].clone(),left)?;source.set("quantity",left);
                    s.record_trade((o["charID"].clone(),order.owner.clone()),order.type_id.clone(),take,unit,time,true)?;
                    if !o["charID"].is_none(){let (added,changed)=s.deposit(o["charID"].clone(),order.type_id.clone(),take)?;let entry=result.others.entry(o["charID"].int()).or_default();entry.0.extend(added);entry.1.extend(changed);}
                    result.earned+=unit as f64*take as f64;
                }
                let left=o["volRemaining"].int()-take;
                if left<=0{s.exec("DELETE FROM market_orders WHERE \"orderID\"=$1",&[o["orderID"].clone()])?;}else{s.exec("UPDATE market_orders SET \"volRemaining\"=$1 WHERE \"orderID\"=$2",&[left.into(),o["orderID"].clone()])?;}
                remaining-=take;result.filled+=take;
            }
            if remaining>0 && days>0 {
                let mut escrow=V::None;let mut stand=true;
                if order.bid {let cost=order.price as f64*remaining as f64;
                    if s.balance(order.owner.clone())?<cost {if result.filled==0{return Err(refusal("You do not have enough ISK to place that order."));}stand=false;}
                    else{s.money(order.owner.clone(),-cost)?;result.spent+=cost;}
                }else if source["singleton"].truth() || source["quantity"].int()<=remaining {
                    escrow=source["itemID"].clone();s.exec("UPDATE inventory SET flag=$1 WHERE \"itemID\"=$2",&[s.p("flagMarket"),escrow.clone()])?;
                }else{
                    s.quantity(source["itemID"].clone(),source["quantity"].int()-remaining)?;
                    escrow=s.next_id("inventory","itemID",s.c("ITEM_ID_BASE").int())?.into();
                    s.insert("inventory",&d!["itemID"=>escrow.clone(),"ownerID"=>order.owner.clone(),"locationID"=>s.c("STATION_ID"),"typeID"=>order.type_id.clone(),"flag"=>s.p("flagMarket"),"quantity"=>remaining,"singleton"=>0],false)?;
                }
                if stand{if s.data["market._RANGE_RANK"].key(&order.range).is_none(){order.range=s.p("rangeStation");}result.order=Some(s.stand(&order,remaining,days,escrow,time)?);}
            }else if remaining>0 && result.filled==0 {
                return Err(refusal(if broke{"You do not have enough ISK."}else if order.bid{"Nobody is selling at that price or less."}else{"Nobody is buying at that price or more."}));
            }
            if !order.bid {if s.item(source["itemID"].clone())?.is_none(){result.removed.push(source["itemID"].clone());}else{result.changed.push(source["itemID"].clone());}}
            Ok(result)
        })
    }
}
