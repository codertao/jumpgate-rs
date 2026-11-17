//! NPC simulation. Knows ships, hulls and behaviours, and nothing about the
//! protocol, the network or players: it only produces [`ShipView`]s. The server
//! decides who sees them and how they're encoded.

mod orbit;

pub use orbit::Orbit;

use crate::ship::{Hull, ShipView};

/// Stable NPC identifier, local to the simulation.
pub type NpcId = u32;

/// Decides how one NPC moves. Implementations update the ship in place each tick.
pub trait Behaviour: Send {
    fn step(&mut self, ship: &mut ShipView, hull: &Hull, dt: f32);
}

struct Npc {
    id: NpcId,
    hull: Hull,
    ship: ShipView,
    brain: Box<dyn Behaviour>,
}

#[derive(Default)]
pub struct NpcSim {
    npcs: Vec<Npc>,
    next_id: NpcId,
}

impl NpcSim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an NPC flying `hull` in `loc`, driven by `brain`. The behaviour places
    /// it on its first tick.
    pub fn spawn(&mut self, callsign: &str, hull: Hull, loc: u16, brain: Box<dyn Behaviour>) -> NpcId {
        let id = self.next_id;
        self.next_id += 1;
        let ship = ShipView {
            loc,
            hull_type: hull.type_id,
            callsign: callsign.to_string(),
            mass: hull.mass,
            pos: [0.0; 3],
            vel: [0.0; 3],
            fwd: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
            thrust: 0.0,
            armour: 1.0,
            armour_max: hull.armour,
        };
        let mut npc = Npc { id, hull, ship, brain };
        npc.brain.step(&mut npc.ship, &npc.hull, 0.0);
        self.npcs.push(npc);
        id
    }

    pub fn tick(&mut self, dt: f32) {
        for n in &mut self.npcs {
            n.brain.step(&mut n.ship, &n.hull, dt);
        }
    }

    pub fn ships(&self) -> impl Iterator<Item = (NpcId, &ShipView)> {
        self.npcs.iter().map(|n| (n.id, &n.ship))
    }
}
