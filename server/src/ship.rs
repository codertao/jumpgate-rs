//! Plain ship data shared between the NPC simulation and the server.
//!
//! This is the only thing the two sides agree on: `npc` produces `ShipView`s,
//! `game` turns them into wire messages. Neither depends on the other.

pub type Vec3 = [f32; 3];

/// Static stats for a hull, from `notes/extracts/ship_types.csv`.
#[derive(Clone, Debug, PartialEq)]
pub struct Hull {
    /// Ship-type id as the client knows it (0x05 @0x46).
    pub type_id: u16,
    pub name: String,
    pub mass: f32,
    /// Quadratic drag coefficient: deceleration = drag · speed² / mass (physics.md §3).
    pub drag: f32,
    /// Hull armour points (ship_types.csv `armour`).
    pub armour: f32,
}

impl Hull {
    /// Thrust that holds `speed` against drag. Clients dead-reckon remote ships
    /// with the same law, so publishing this keeps them from slowing between
    /// updates.
    pub fn cruise_thrust(&self, speed: f32) -> f32 {
        self.drag * speed * speed
    }
}

/// Everything a client needs to draw and dead-reckon one ship.
#[derive(Clone, Debug, PartialEq)]
pub struct ShipView {
    pub loc: u16,
    pub hull_type: u16,
    pub callsign: String,
    pub mass: f32,
    pub pos: Vec3,
    pub vel: Vec3,
    /// Unit forward and up vectors.
    pub fwd: Vec3,
    pub up: Vec3,
    /// Effective thrust (thrust × throttle), pushed along `fwd`.
    pub thrust: f32,
    /// Armour remaining as a fraction 0..1, and the hull's full armour points.
    pub armour: f32,
    pub armour_max: f32,
}

pub fn length(v: Vec3) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

pub fn normalize(v: Vec3) -> Vec3 {
    let l = length(v);
    if l > 1e-6 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 0.0, 1.0]
    }
}
