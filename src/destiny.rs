use crate::{d, l, pickle::Value as V, state::State, t};
use std::time::{Duration, Instant};
pub const RAT_ID: i64 = 94000001;
#[derive(Debug)]
pub struct Rat {
    pub hp: [f64; 3],
    pub due: Option<Instant>,
}
impl Default for Rat {
    fn default() -> Self {
        Self {
            hp: [400., 400., 200.],
            due: None,
        }
    }
}
impl Rat {
    pub fn alive(&self) -> bool {
        self.hp[2] > 0.
    }
    pub fn hit(&mut self) -> bool {
        let mut damage: f64 = 25.;
        for layer in &mut self.hp {
            let taken = damage.min(*layer);
            *layer -= taken;
            damage -= taken;
            if damage == 0. {
                break;
            }
        }
        self.due = Some(Instant::now() + Duration::from_secs(if self.alive() { 2 } else { 15 }));
        !self.alive()
    }
    pub fn damage(&self, s: &State) -> V {
        l![
            t![
                self.hp[0] / 400.,
                3600. * 1e7 / s.p("dgmTauConstant").float() / 0.001
            ],
            self.hp[1] / 400.,
            self.hp[2] / 200.
        ]
    }
}
pub fn ball(id: V, pos: [f64; 3], radius: f64, mass: f64, name: V, free: bool) -> V {
    let state = d!["id"=>id,"mass"=>mass,"radius"=>radius,"maxVelocity"=>if free{300.}else{0.},"isFree"=>i32::from(free),"isGlobal"=>1,"isMassive"=>1,"isInteractive"=>1,
        "x"=>pos[0],"y"=>pos[1],"z"=>pos[2],"vx"=>0.0,"vy"=>0.0,"vz"=>0.0,"agility"=>1.0,"speedFraction"=>0.0,"name"=>name];
    V::Reduce {
        module: "triPickle".into(),
        name: "MakeDestinyBall".into(),
        args: vec![state],
    }
}
fn slim(s: &State, id: V, rat: bool) -> V {
    let state = d!["itemID"=>id,"charID"=>V::None,"ownerID"=>if rat{s.c("PIRATE_OWNER_ID")}else{V::None},"groupID"=>s.c(if rat{"GROUP_PIRATE_FRIGATE"}else{"GROUP_FRIGATE"}),
        "categoryID"=>s.c(if rat{"CATEGORY_ENTITY"}else{"CATEGORY_SHIP"}),"typeID"=>s.c(if rat{"RAT_TYPE_ID"}else{"SHIP_TYPE_ID"}),"name"=>if rat{"Pirate Frigate"}else{"Your ship"},
        "graphicID"=>s.c(if rat{"RAT_GRAPHIC_ID"}else{"SHIP_GRAPHIC_ID"}),"modules"=>l![],"jumps"=>l![],"orbitID"=>V::None];
    V::Instance {
        module: "foo".into(),
        name: "SlimItem".into(),
        args: vec![],
        state: Box::new(state),
    }
}
pub fn rat_objects(s: &State) -> (V, V) {
    (
        ball(
            RAT_ID.into(),
            [6000., 0., 9000.],
            30.,
            1e6,
            "Pirate Frigate".into(),
            false,
        ),
        slim(s, RAT_ID.into(), true),
    )
}
pub fn objects(s: &State, ship: V, rat: Option<&Rat>) -> (Vec<V>, Vec<V>) {
    let sky = s.template("sky");
    let mut balls = sky[0].seq().to_vec();
    let mut slims = sky[1].seq().to_vec();
    if !ship.is_none() {
        balls.push(ball(
            ship.clone(),
            [0., 0., 2500.],
            30.,
            1e6,
            "Your ship".into(),
            true,
        ));
        slims.push(slim(s, ship, false));
    }
    if rat.is_some_and(Rat::alive) {
        let (b, s) = rat_objects(s);
        balls.push(b);
        slims.push(s);
    }
    (balls, slims)
}
pub fn undock(s: &State, ship: V, rat: Option<&Rat>) -> V {
    let (balls, slims) = objects(s, ship.clone(), rat);
    let mut damage = d![];
    if let Some(r) = rat.filter(|r| r.alive()) {
        damage.set(RAT_ID, r.damage(s));
    }
    l![t![
        1,
        t![
            "SetState",
            t![V::List(balls), V::List(slims), 1, ship, V::None, damage]
        ]
    ]]
}
pub fn action(epoch: Instant, name: &str, mut args: Vec<V>) -> V {
    let stamp = 1 + (epoch.elapsed().as_millis() / 500) as i64;
    if matches!(name, "FollowBall" | "Orbit") {
        args.push(stamp.into());
    }
    l![t![stamp, t![name, V::Tuple(args)]]]
}
pub fn layout(s: &State, ship: V, rat: Option<&Rat>) -> V {
    let mut out = d![];
    for b in objects(s, ship, rat).0 {
        if let V::Reduce { args, .. } = b {
            let p = &args[0];
            out.set(
                p["id"].clone(),
                t![
                    p["x"].clone(),
                    p["y"].clone(),
                    p["z"].clone(),
                    p["radius"].clone()
                ],
            );
        }
    }
    out
}
