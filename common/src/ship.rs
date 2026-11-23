//! Plain ship data shared between the NPC simulation and the server.
//!
//! This is the only thing the two sides agree on: `npc` produces `ShipView`s,
//! `game` turns them into wire messages. Neither depends on the other.

pub type Vec3 = [f32; 3];

/// Static stats for a hull, from `notes/extracts/ship_types.csv`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Hull {
    /// Ship-type id as the client knows it (0x05 @0x46).
    pub type_id: u16,
    pub name: String,
    pub mass: f32,
    /// Quadratic drag coefficient: deceleration = drag · speed² / mass (physics.md §3).
    pub drag: f32,
    /// Hull armour points (ship_types.csv `armour`).
    pub armour: f32,
    /// Turn-rate coefficients (ship_types.csv `pitch`/`yaw`/`roll`). At full
    /// stick the client turns at about 1000 × rate rad/s (physics.md §4).
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
    /// Number of engine mounts: thrust = engines × engine thrust (physics.md §5).
    pub engines: u8,
    /// Ship-type field +0x142 (`f142`): how far down the launch tube the ship
    /// appears (notes/launch.md).
    pub tube_offset: f32,
    /// Ship-type field +0x144 (`size`): half of it counts against the launch
    /// tube's radius.
    pub size: f32,
    /// Collision extents right/up/forward (+0x158/15c/160). The own-ship probe
    /// sweeps the centre and centre ± ½·extent along each axis (physics.md §6).
    pub ext: Vec3,
}

impl Hull {
    /// Thrust that holds `speed` against drag. Clients dead-reckon remote ships
    /// with the same law, so publishing this keeps them from slowing between
    /// updates.
    pub fn cruise_thrust(&self, speed: f32) -> f32 {
        self.drag * speed * speed
    }

    /// Distance from the tube's inner end to where a launch places the ship:
    /// `f142` × 5 for types 241–242, × 4 for 246–248, × 2 for 120–121
    /// (`0x9d363a`…).
    pub fn launch_depth(&self) -> f32 {
        let k = match self.type_id {
            241 | 242 => 5.0,
            246..=248 => 4.0,
            120 | 121 => 2.0,
            _ => 1.0,
        };
        self.tube_offset * k
    }
}

/// A shield item (shields.csv; table `0x146cd0c`, 0x18-byte rows).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Shield {
    /// Points per second (+0xc).
    pub regen: f32,
    /// Full charge (+0x14).
    pub capacity: f32,
}

/// Hull points per point of ship-type armour: the 0x05 @0xa4 hull and the
/// 0x0E report both measure against `armour × 3000` (message-types "0x0E").
pub const HULL_PER_ARMOUR: f32 = 3000.0;

/// Shields for a ship whose fitted shield isn't known (server-driven ships):
/// armour × 1000, which is what a captured Thunder (armour 24000) reported.
pub const DEFAULT_SHIELD_PER_ARMOUR: f32 = 1000.0;

/// Fallback regen when no shield matches, as the client's remote-ship
/// prediction does (`0x433340`, constant `0xaeaf08`).
pub const DEFAULT_SHIELD_REGEN: f32 = 1000.0;

/// Shields and hull of one ship, with the stock client's rules:
/// - damage (S→C 0x03, impacts) comes off the shields first and the overflow
///   off the hull; hull ≤ 0 is death (0x03 handler `0x9d1463`);
/// - shields regenerate at the shield's `regen` per second up to capacity.
///   That's the rule every client uses to predict remote ships (`0x433309`),
///   so an agent following it never disagrees with what players see.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vitals {
    pub shields: f32,
    pub shields_max: f32,
    pub regen: f32,
    pub hull: f32,
    pub hull_max: f32,
}

impl Vitals {
    /// Full shields and hull for `hull` with `shield` fitted (none: no shields).
    pub fn new(hull: &Hull, shield: Option<Shield>) -> Vitals {
        let s = shield.unwrap_or_default();
        let hull_max = hull.armour * HULL_PER_ARMOUR;
        Vitals { shields: s.capacity, shields_max: s.capacity, regen: s.regen, hull: hull_max, hull_max }
    }

    /// For ships with no known loadout: shields scaled from the armour.
    pub fn assumed(hull: &Hull) -> Vitals {
        let cap = hull.armour * DEFAULT_SHIELD_PER_ARMOUR;
        Vitals::new(hull, Some(Shield { regen: DEFAULT_SHIELD_REGEN, capacity: cap }))
    }

    pub fn is_dead(&self) -> bool {
        self.hull <= 0.0
    }

    /// Apply damage; true if this hit was the killing one.
    pub fn damage(&mut self, amount: f32) -> bool {
        if self.is_dead() || !(amount > 0.0) {
            return false;
        }
        self.shields -= amount;
        if self.shields < 0.0 {
            self.hull += self.shields;
            self.shields = 0.0;
        }
        self.is_dead()
    }

    pub fn regenerate(&mut self, dt: f32) {
        if !self.is_dead() && self.shields < self.shields_max {
            self.shields = (self.shields + self.regen * dt).min(self.shields_max);
        }
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
    /// Shields now and at full, and hull points (0x05 @0x9c/@0xa0/@0xa4).
    pub shields: f32,
    pub shields_max: f32,
    pub hull: f32,
    /// Pilot rank, shown as the level on other clients (0x05 @0x14).
    pub rank: u8,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn storm() -> Hull {
        Hull { type_id: 90, armour: 4000.0, ..Default::default() }
    }

    #[test]
    fn damage_drains_shields_then_hull() {
        let mut v = Vitals::new(&storm(), Some(Shield { regen: 73500.0, capacity: 7.0e6 }));
        assert_eq!((v.shields, v.hull), (7.0e6, 12.0e6));
        assert!(!v.damage(5.0e6));
        assert_eq!((v.shields, v.hull), (2.0e6, 12.0e6));
        assert!(!v.damage(4.0e6));
        assert_eq!((v.shields, v.hull), (0.0, 10.0e6));
        assert!(!v.damage(f32::NAN) && !v.damage(-1.0));
        assert!(v.damage(10.0e6), "the killing hit");
        assert!(!v.damage(1.0), "already dead");
    }

    #[test]
    fn shields_regenerate_up_to_capacity() {
        let mut v = Vitals::new(&storm(), Some(Shield { regen: 73500.0, capacity: 7.0e6 }));
        v.damage(1.0e6);
        v.regenerate(2.0);
        assert_eq!(v.shields, 6.0e6 + 147000.0);
        v.regenerate(100.0);
        assert_eq!(v.shields, 7.0e6);
        let none = Vitals::new(&storm(), None);
        assert_eq!((none.shields_max, none.hull), (0.0, 12.0e6));
    }
}
