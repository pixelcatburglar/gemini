//! Server-owned flight state. The client predicts Destiny motion; snapshots and
//! persistence always use this state, never client-supplied ship positions.
use crate::{
    d, destiny,
    game::{Client, Game},
    pickle::Value as V,
    state::{Result, State, user},
    t,
};
use std::time::{Duration, Instant};
pub type Point = [f64; 3];
pub fn length(v: Point) -> f64 {
    v[0].hypot(v[1]).hypot(v[2])
}
pub fn sub(a: Point, b: Point) -> Point {
    std::array::from_fn(|i| a[i] - b[i])
}
pub fn unit(v: Point) -> Point {
    let n = length(v);
    if n > 0. {
        v.map(|x| x / n)
    } else {
        [0., 0., 1.]
    }
}
pub fn coordinates(a: &V) -> Result<Point> {
    let v = [a[0].float(), a[1].float(), a[2].float()];
    if (0..3).any(|i| !matches!(a[i], V::Int(_) | V::Float(_)))
        || v.iter().any(|x| !x.is_finite() || x.abs() >= 1e18)
    {
        return Err(user(
            "Coordinates must be finite and within the solar system.",
        ));
    }
    Ok(v)
}
pub fn arrival(from: Point, target: Point, clearance: f64) -> Point {
    let delta = sub(from, target);
    let direction = unit(delta);
    let distance = clearance.min(length(delta));
    std::array::from_fn(|i| target[i] + direction[i] * distance)
}
#[derive(Clone, Debug)]
pub enum Motion {
    Stopped,
    Direction(Point),
    Point(Point),
    Follow {
        target: V,
        center: Point,
        radius: f64,
        distance: f64,
        orbit: bool,
    },
    Warp(Point),
}
#[derive(Clone, Debug)]
pub struct Flight {
    pub position: Point,
    pub velocity: Point,
    pub speed: f64,
    pub max_speed: f64,
    pub radius: f64,
    pub mass: f64,
    pub agility: f64,
    pub motion: Motion,
    pub pending_dock: Option<V>,
    pub updated: Instant,
    pub saved: Instant,
}
impl Flight {
    pub fn advance(&mut self, seconds: f64) {
        // Bounded substeps make integration insensitive to the server tick rate.
        let mut remaining = seconds.max(0.);
        while remaining > 0. {
            let dt = remaining.min(0.1);
            remaining -= dt;
            let desired = match &self.motion {
                Motion::Stopped => [0.; 3],
                Motion::Direction(d) => unit(*d).map(|x| x * self.max_speed * self.speed),
                Motion::Point(p) => unit(sub(*p, self.position)).map(|x| {
                    x * (length(sub(*p, self.position)) / dt).min(self.max_speed * self.speed)
                }),
                Motion::Follow {
                    center,
                    radius,
                    distance,
                    orbit,
                    ..
                } => {
                    let delta = sub(*center, self.position);
                    let separation = length(delta);
                    let offset = separation - radius - self.radius - distance;
                    let radial = unit(delta);
                    let mut tangent = unit([-radial[2], 0., radial[0]]);
                    if radial[0].abs() + radial[2].abs() < 1e-8 {
                        tangent = [1., 0., 0.];
                    }
                    let correction = (offset / 100.).clamp(-1., 1.);
                    let direction = std::array::from_fn(|i| {
                        radial[i] * correction + if *orbit { tangent[i] } else { 0. }
                    });
                    let factor = if *orbit { 1. } else { correction.abs() };
                    unit(direction).map(|x| x * self.max_speed * self.speed * factor)
                }
                Motion::Warp(destination) => {
                    let delta = sub(*destination, self.position);
                    let distance = length(delta);
                    // A bounded cruise with gradual acceleration/deceleration.
                    let speed = (length(self.velocity) + 3e10 * dt)
                        .min(4.5e11)
                        .min((2. * 3e10 * distance).sqrt());
                    if distance <= speed * dt || distance < 1. {
                        self.position = *destination;
                        self.velocity = [0.; 3];
                        self.motion = Motion::Stopped;
                        continue;
                    }
                    self.velocity = unit(delta).map(|x| x * speed);
                    for i in 0..3 {
                        self.position[i] += self.velocity[i] * dt;
                    }
                    continue;
                }
            };
            let tau = (self.mass * self.agility / 1e6).clamp(0.1, 60.);
            let decay = (-dt / tau).exp();
            for (i, target) in desired.iter().enumerate() {
                // Exact integration for a constant desired velocity per substep.
                self.position[i] += target * dt + (self.velocity[i] - target) * tau * (1. - decay);
                self.velocity[i] = target + (self.velocity[i] - target) * decay;
            }
            if matches!(self.motion, Motion::Stopped) && length(self.velocity) < 0.01 {
                self.velocity = [0.; 3];
            }
        }
    }
    pub fn update(&mut self) {
        let now = Instant::now();
        self.advance(now.duration_since(self.updated).as_secs_f64().min(60.));
        self.updated = now;
    }
    pub fn command(&self) -> (&'static str, Vec<V>) {
        match &self.motion {
            Motion::Stopped => ("Stop", vec![]),
            Motion::Direction(p) => ("GotoDirection", p.map(V::from).to_vec()),
            Motion::Point(p) => ("GotoPoint", p.map(V::from).to_vec()),
            Motion::Follow {
                target,
                distance,
                orbit,
                ..
            } => (
                if *orbit { "Orbit" } else { "FollowBall" },
                vec![target.clone(), (*distance).into()],
            ),
            Motion::Warp(p) => ("WarpTo", p.map(V::from).to_vec()),
        }
    }
}
impl State {
    pub(crate) fn require_space(&self, c: &Client) -> Result<()> {
        let loc = self.location(c.char_id())?;
        let item = self.item(c.ship())?;
        if c.account.is_none()
            || c.char_id().is_none()
            || !self.owns(c.account.clone(), c.char_id())?
            || c.ship().is_none()
            || !c.session["stationid"].is_none()
            || !loc["stationID"].is_none()
            || loc["shipID"] != c.ship()
            || item["ownerID"] != c.char_id()
            || item["locationID"] != loc["solarSystemID"]
        {
            return Err(user("You are not flying this ship in space."));
        }
        Ok(())
    }
    pub(crate) fn save_flight(&self, ship: V, f: &Flight) -> Result<()> {
        self.exec("INSERT INTO ship_position VALUES($1,$2,$3,$4) ON CONFLICT(\"shipID\") DO UPDATE SET x=excluded.x,y=excluded.y,z=excluded.z",&[ship,f.position[0].into(),f.position[1].into(),f.position[2].into()])?;
        Ok(())
    }
    pub(crate) fn load_flight(&self, ship: V) -> Result<Flight> {
        let position = self.one(
            "SELECT * FROM ship_position WHERE \"shipID\"=$1",
            std::slice::from_ref(&ship),
        )?;
        let stats = self.data["catalog.SHIP_DOGMA_STATS"].key(&self.ship_type(ship)?);
        let positive = |key: &str, default: f64| {
            let n = stats[key].float();
            if n.is_finite() && n > 0. { n } else { default }
        };
        Ok(Flight {
            position: if position.is_none() {
                [0., 0., 2500.]
            } else {
                [
                    position["x"].float(),
                    position["y"].float(),
                    position["z"].float(),
                ]
            },
            velocity: [0.; 3],
            speed: 1.,
            max_speed: positive("maxVelocity", 300.),
            radius: positive("radius", 30.),
            mass: positive("mass", 1e6),
            agility: positive("agility", 1.),
            motion: Motion::Stopped,
            pending_dock: None,
            updated: Instant::now(),
            saved: Instant::now(),
        })
    }
}
impl Game {
    pub(crate) fn space_event(
        &mut self,
        c: &mut Client,
        name: &str,
        args: Vec<V>,
        defer: bool,
    ) -> Result<()> {
        if let Some(b) = c.ballpark.as_ref().and_then(|id| c.objects.get(id)) {
            c.broadcast(
                "OnDestinyUpdate",
                t![destiny::action(b.epoch, name, args.clone())],
                defer,
            )?;
        }
        for other in self.clients.values_mut().filter(|o| {
            !o.closed
                && o.session["solarsystemid"] == c.session["solarsystemid"]
                && o.session["stationid"].is_none()
        }) {
            if let Some(b) = other.ballpark.as_ref().and_then(|id| other.objects.get(id)) {
                other.broadcast(
                    "OnDestinyUpdate",
                    t![destiny::action(b.epoch, name, args.clone())],
                    false,
                )?;
            }
        }
        Ok(())
    }
    pub(crate) fn board(&mut self, c: &mut Client, ship: V) -> Result<()> {
        self.state.transaction(|s| {
            let station = s.require_station(c)?;
            if !ship.is_none() {
                let item = s.item(ship.clone())?;
                if item["ownerID"] != c.char_id()
                    || item["locationID"] != station
                    || item["flag"] != s.c("FLAG_HANGAR")
                    || s.category(item["typeID"].clone()) != s.c("CATEGORY_SHIP")
                {
                    return Err(user("Select a ship in your station hangar."));
                }
            }
            s.exec(
                "UPDATE character_location SET \"shipID\"=$1 WHERE \"characterID\"=$2",
                &[ship.clone(), c.char_id()],
            )?;
            Ok(())
        })?;
        c.change_session(d!["shipid"=>ship.clone(),"locrole"=>if ship.is_none(){V::None}else{self.state.c("FLAG_PILOT")}])
    }
    pub(crate) fn undock_ship(&mut self, c: &mut Client) -> Result<()> {
        let mut flight = self.state.load_flight(c.ship())?;
        let station = self.state.require_station(c)?;
        let layout = destiny::layout(&self.state, None);
        let location = layout.key(&station);
        if !location.is_none() {
            flight.position = [
                location[0].float(),
                location[1].float(),
                location[2].float() + location[3].float() + flight.radius + 1000.,
            ];
        }
        self.state.transaction(|s| {
            s.require_station(c)?;
            let item=s.item(c.ship())?;
            if c.ship().is_none() || item["ownerID"]!=c.char_id() || item["locationID"]!=station || item["flag"]!=s.c("FLAG_HANGAR") || s.category(item["typeID"].clone())!=s.c("CATEGORY_SHIP") {return Err(user("Board a ship in this station before undocking."));}
            s.exec("UPDATE character_location SET \"stationID\"=NULL,\"shipID\"=$1 WHERE \"characterID\"=$2",&[c.ship(),c.char_id()])?;
            s.exec("UPDATE inventory SET \"locationID\"=$1,flag=0 WHERE \"itemID\"=$2",&[c.session["solarsystemid"].clone(),c.ship()])?;
            s.save_flight(c.ship(),&flight)
        })?;
        c.flight = Some(flight);
        c.targets.clear();
        c.change_session(d!["stationid"=>V::None,"stationItem"=>V::None,"locationid"=>c.session["solarsystemid"].clone()])
    }
    pub(crate) fn dock_ship(&mut self, c: &mut Client, station: V) -> Result<()> {
        self.state.transaction(|s| {
            s.require_space(c)?;
            if station != s.c("STATION_ID") {
                return Err(user("That object is not a docking station."));
            }
            s.exec(
                "UPDATE character_location SET \"stationID\"=$1 WHERE \"characterID\"=$2",
                &[station.clone(), c.char_id()],
            )?;
            s.exec(
                "UPDATE inventory SET \"locationID\"=$1,flag=$2 WHERE \"itemID\"=$3",
                &[station.clone(), s.c("FLAG_HANGAR"), c.ship()],
            )?;
            if let Some(f) = &c.flight {
                s.save_flight(c.ship(), f)?;
            }
            Ok(())
        })?;
        self.space_event(c, "RemoveBall", vec![c.ship(), (-1).into()], true)?;
        if let Some(id) = c.ballpark.take() {
            c.objects.remove(&id);
        }
        c.flight = None;
        c.targets.clear();
        c.broadcast("OnTarget", t!["clear"], true)?;
        c.change_session(d!["stationid"=>station.clone(),"locationid"=>station,"stationItem"=>self.state.template("station_item")])
    }
    pub(crate) fn tick_flights(&mut self) -> Result<()> {
        let ids: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| c.flight.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let mut c = self.clients.remove(&id).unwrap();
            let result = (|| -> Result<()> {
                let f = c.flight.as_mut().unwrap();
                if let Motion::Follow {
                    target,
                    center,
                    radius,
                    ..
                } = &mut f.motion
                {
                    if let Some(other) = self.clients.values().find(|o| {
                        o.ship() == *target
                            && o.session["solarsystemid"] == c.session["solarsystemid"]
                            && o.session["stationid"].is_none()
                    }) {
                        if let Some(flight) = &other.flight {
                            let mut flight = flight.clone();
                            flight.update();
                            *center = flight.position;
                            *radius = flight.radius;
                        }
                    } else if destiny::layout(&self.state, Some(&self.rat))
                        .key(target)
                        .is_none()
                    {
                        f.motion = Motion::Stopped;
                    }
                }
                f.update();
                if f.saved.elapsed() >= Duration::from_secs(1) {
                    self.state.save_flight(c.session["shipid"].clone(), f)?;
                    f.saved = Instant::now();
                }
                if let Some(station) = f.pending_dock.clone() {
                    let layout = destiny::layout(&self.state, None).key(&station);
                    if length(sub(
                        f.position,
                        [layout[0].float(), layout[1].float(), layout[2].float()],
                    )) <= layout[3].float()
                        + f.radius
                        + self.state.p("maxDockingDistance").float()
                    {
                        self.dock_ship(&mut c, station)?;
                        c.out.append(&mut c.deferred);
                    }
                }
                Ok(())
            })();
            self.clients.insert(id, c);
            result?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn flight() -> Flight {
        Flight {
            position: [0.; 3],
            velocity: [0.; 3],
            speed: 1.,
            max_speed: 300.,
            radius: 30.,
            mass: 1e6,
            agility: 1.,
            motion: Motion::Direction([1., 0., 0.]),
            pending_dock: None,
            updated: Instant::now(),
            saved: Instant::now(),
        }
    }
    #[test]
    fn movement_accelerates_stops_and_is_tick_rate_independent() {
        let mut slow = flight();
        let mut fast = slow.clone();
        slow.advance(10.);
        for _ in 0..100 {
            fast.advance(0.1);
        }
        assert!(length(sub(slow.position, fast.position)) < 1e-6);
        assert!(slow.velocity[0] > 299. && slow.velocity[0] <= 300.);
        assert!(slow.position[0] > 2600. && slow.position[0] < 3000.);
        slow.motion = Motion::Stopped;
        slow.advance(15.);
        assert_eq!(slow.velocity, [0.; 3]);
    }
    #[test]
    fn warp_arrives_on_the_near_side_from_the_current_position() {
        assert_eq!(
            arrival([100., 0., 0.], [1000., 0., 0.], 100.),
            [900., 0., 0.]
        );
        assert_eq!(
            arrival([2000., 0., 0.], [1000., 0., 0.], 100.),
            [1100., 0., 0.]
        );
        assert_eq!(
            arrival([1050., 0., 0.], [1000., 0., 0.], 100.),
            [1050., 0., 0.]
        );
        let mut ship = flight();
        ship.motion = Motion::Warp([1e10, 0., 0.]);
        ship.advance(30.);
        assert_eq!(ship.position, [1e10, 0., 0.]);
        assert!(matches!(ship.motion, Motion::Stopped));
    }
    #[test]
    fn coordinate_validation_rejects_nan_infinity_strings_and_missing_values() {
        for bad in [
            t![f64::NAN, 0., 0.],
            t![f64::INFINITY, 0., 0.],
            t!["1", 0, 0],
            t![0, 0],
            t![1e20, 0, 0],
        ] {
            assert!(coordinates(&bad).is_err());
        }
        assert_eq!(coordinates(&t![1, 2, 3]).unwrap(), [1., 2., 3.]);
    }
}
