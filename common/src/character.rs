//! A pilot as the client sees it: what the 0x06 character record, 0x45
//! credits/items and 0x92 experience carry. Field meanings are from the 0x06
//! handler `0x9d205f` (message-types.md "0x06 character record").
//!
//! Item references are `(category, index)`, the same pair the client uses in
//! the 0x45 item list and in C→S 0x66 fitting changes. Categories (items.md):
//! 0 guns, 1 missiles, 3 commodities, 5 engines, 6 powerplants, 7 radars,
//! 8 ECMs, 9 shields, 10 capacitors, 11 devices. Index is the position within
//! the category (the market listing entry).

use serde::{Deserialize, Serialize};

pub const EMPTY: u8 = 0xfe;
pub const ITEM_SLOTS: usize = 48;

pub const CAT_GUN: u8 = 0;
pub const CAT_MISSILE: u8 = 1;
pub const CAT_COMMODITY: u8 = 3;
pub const CAT_ENGINE: u8 = 5;
pub const CAT_POWERPLANT: u8 = 6;
pub const CAT_RADAR: u8 = 7;
pub const CAT_ECM: u8 = 8;
pub const CAT_SHIELD: u8 = 9;
pub const CAT_CAPACITOR: u8 = 10;
pub const CAT_DEVICE: u8 = 11;

/// Fitted equipment, as the 30 bytes at 0x06 @0x70: 8 guns, 8 missile racks,
/// 8 devices, then engine, powerplant, radar, ECM, shield, capacitor. Each byte
/// is an index within its category, `0xfe` = empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fittings {
    pub guns: [u8; 8],
    pub missiles: [u8; 8],
    pub devices: [u8; 8],
    pub engine: u8,
    pub powerplant: u8,
    pub radar: u8,
    pub ecm: u8,
    pub shield: u8,
    pub capacitor: u8,
}

impl Default for Fittings {
    fn default() -> Self {
        Fittings {
            guns: [EMPTY; 8],
            missiles: [EMPTY; 8],
            devices: [EMPTY; 8],
            engine: EMPTY,
            powerplant: EMPTY,
            radar: EMPTY,
            ecm: EMPTY,
            shield: EMPTY,
            capacitor: EMPTY,
        }
    }
}

impl Fittings {
    pub const LEN: usize = 30;

    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut b = [EMPTY; Self::LEN];
        b[0..8].copy_from_slice(&self.guns);
        b[8..16].copy_from_slice(&self.missiles);
        b[16..24].copy_from_slice(&self.devices);
        b[24..30].copy_from_slice(&[self.engine, self.powerplant, self.radar, self.ecm, self.shield, self.capacitor]);
        b
    }

    /// The fitted byte for a 0x66 `(category, ship slot)`, if that slot exists.
    pub fn slot_mut(&mut self, cat: u8, slot: u8) -> Option<&mut u8> {
        let s = slot as usize;
        match cat {
            CAT_GUN => self.guns.get_mut(s),
            CAT_MISSILE => self.missiles.get_mut(s),
            CAT_DEVICE => self.devices.get_mut(s),
            CAT_ENGINE => Some(&mut self.engine),
            CAT_POWERPLANT => Some(&mut self.powerplant),
            CAT_RADAR => Some(&mut self.radar),
            CAT_ECM => Some(&mut self.ecm),
            CAT_SHIELD => Some(&mut self.shield),
            CAT_CAPACITOR => Some(&mut self.capacitor),
            _ => None,
        }
    }
}

/// One 0x45 item-list slot: `[u8 category][u8 index][cc cc][f32 quantity]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub cat: u8,
    pub idx: u8,
    pub qty: f32,
}

impl Item {
    pub const NONE: Item = Item { cat: EMPTY, idx: EMPTY, qty: 0.0 };

    pub fn is_empty(&self) -> bool {
        self.cat == EMPTY || self.qty <= 0.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Character {
    pub callsign: String,
    /// 0x06 @1: 1 Solrain, 2 Octavius, 3 Quantar (locations.csv `faction`).
    pub faction: u8,
    /// 0x06 @2: rank, shown with the faction's rank titles.
    pub rank: u8,
    /// 0x06 @0x42: ship type (ship_types.csv).
    pub ship_type: u8,
    pub fittings: Fittings,
    /// Cargo and unfitted equipment (0x45 / 0x06 @0x9c).
    pub items: Vec<Item>,
    pub credits: u64,
    pub experience: u32,
}

impl Character {
    /// The item list as exactly [`ITEM_SLOTS`] slots.
    /// Always gapless (see [`take_item`](Self::take_item)), so a list saved
    /// with holes by an older server is shown closed up.
    pub fn item_slots(&self) -> [Item; ITEM_SLOTS] {
        let mut s = [Item::NONE; ITEM_SLOTS];
        for (d, i) in s.iter_mut().zip(self.items.iter().filter(|i| !i.is_empty())) {
            *d = *i;
        }
        s
    }

    /// Exactly [`ITEM_SLOTS`] slots, items first with no gaps, so slot
    /// numbers agree with what [`item_slots`](Self::item_slots) sent.
    fn ensure_slots(&mut self) {
        self.items.retain(|i| !i.is_empty());
        self.items.resize(ITEM_SLOTS, Item::NONE);
    }

    /// Add `qty` of `(cat, idx)`: onto an existing stack, else the first free
    /// slot. False if the list is full.
    pub fn add_item(&mut self, cat: u8, idx: u8, qty: f32) -> bool {
        self.ensure_slots();
        if let Some(i) = self.items.iter_mut().find(|i| !i.is_empty() && i.cat == cat && i.idx == idx) {
            i.qty += qty;
            return true;
        }
        match self.items.iter_mut().find(|i| i.is_empty()) {
            Some(i) => {
                *i = Item { cat, idx, qty };
                true
            }
            None => false,
        }
    }

    /// Put `qty` of `(cat, idx)` into a specific slot (as 0x66 unfit names
    /// one): stacks if the slot holds the same item, else needs it empty.
    pub fn put_item_at(&mut self, slot: usize, cat: u8, idx: u8, qty: f32) -> bool {
        self.ensure_slots();
        match self.items.get_mut(slot) {
            Some(i) if i.is_empty() => {
                *i = Item { cat, idx, qty };
                true
            }
            Some(i) if i.cat == cat && i.idx == idx => {
                i.qty += qty;
                true
            }
            _ => self.add_item(cat, idx, qty),
        }
    }

    /// Take up to `qty` from a slot; returns the item and amount taken. A slot
    /// that empties is removed and the later items move up one, as the classic
    /// server's list does and the client assumes: in jgcap2 the player sold
    /// slot 1 out, then sold "slot 1" again with the next item's exact
    /// quantity, and each 0x06 that followed showed the list closed up.
    pub fn take_item(&mut self, slot: usize, qty: f32) -> Option<(Item, f32)> {
        self.ensure_slots();
        let i = self.items.get_mut(slot)?;
        if i.is_empty() || qty <= 0.0 {
            return None;
        }
        let took = qty.min(i.qty);
        let item = *i;
        i.qty -= took;
        if i.qty <= 1e-4 {
            self.items.remove(slot);
            self.items.push(Item::NONE);
        }
        Some((item, took))
    }

    /// C→S 0x66 op 0: move the fitted item in `(cat, ship_slot)` to item-list
    /// slot `list_slot`.
    pub fn unfit(&mut self, list_slot: usize, cat: u8, ship_slot: u8) -> bool {
        let Some(f) = self.fittings.slot_mut(cat, ship_slot) else { return false };
        if *f == EMPTY {
            return false;
        }
        let idx = std::mem::replace(f, EMPTY);
        self.put_item_at(list_slot, cat, idx, 1.0)
    }

    /// C→S 0x66 op 1: fit one of item-list slot `list_slot` into
    /// `(cat, ship_slot)`. Whatever was there goes back to the list.
    pub fn fit(&mut self, list_slot: usize, cat: u8, ship_slot: u8) -> bool {
        self.ensure_slots();
        let Some(item) = self.items.get(list_slot).copied() else { return false };
        if item.is_empty() || item.cat != cat || self.fittings.slot_mut(cat, ship_slot).is_none() {
            return false;
        }
        self.take_item(list_slot, 1.0);
        let old = std::mem::replace(self.fittings.slot_mut(cat, ship_slot).unwrap(), item.idx);
        if old != EMPTY {
            self.add_item(cat, old, 1.0);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pilot() -> Character {
        Character {
            callsign: "Test".into(),
            faction: 3,
            rank: 0,
            ship_type: 90,
            fittings: Fittings::default(),
            items: Vec::new(),
            credits: 0,
            experience: 0,
        }
    }

    #[test]
    fn items_stack_and_take() {
        let mut c = pilot();
        assert!(c.add_item(3, 37, 16.0));
        assert!(c.add_item(3, 37, 4.0));
        assert_eq!(c.items[0], Item { cat: 3, idx: 37, qty: 20.0 });
        assert_eq!(c.take_item(0, 25.0), Some((Item { cat: 3, idx: 37, qty: 20.0 }, 20.0)));
        assert!(c.items[0].is_empty());
        assert_eq!(c.take_item(0, 1.0), None);
    }

    /// The capture's ship change (jgcap2 #1361..1399, trimmed): the client
    /// unfits into consecutive list slots (the guns stack in one), fits a gun
    /// back from the stack, then fits the engine from slot 0, after which the
    /// rest of the list moves up.
    #[test]
    fn fit_and_unfit() {
        let mut c = pilot();
        c.fittings.guns[..4].copy_from_slice(&[7; 4]);
        c.fittings.engine = 2;
        assert!(c.unfit(0, CAT_ENGINE, 0));
        for s in 0..4 {
            assert!(c.unfit(1, CAT_GUN, s));
        }
        assert_eq!(c.items[0], Item { cat: 5, idx: 2, qty: 1.0 });
        assert_eq!(c.items[1], Item { cat: 0, idx: 7, qty: 4.0 });
        assert_eq!(c.fittings, Fittings::default());
        assert!(c.fit(1, CAT_GUN, 1));
        assert_eq!(c.fittings.guns[1], 7);
        assert_eq!(c.items[1].qty, 3.0);
        assert!(!c.fit(1, CAT_ENGINE, 0), "category must match");
        assert!(!c.unfit(2, CAT_GUN, 0), "empty slot");
        assert!(c.fit(0, CAT_ENGINE, 0));
        assert_eq!(c.fittings.engine, 2);
        assert_eq!(c.items[0], Item { cat: 0, idx: 7, qty: 3.0 }, "the guns moved up to slot 0");
    }

    #[test]
    fn fittings_bytes_layout() {
        let mut f = Fittings::default();
        f.guns[0] = 7;
        f.devices[0] = 4;
        f.engine = 7;
        f.capacitor = 8;
        let b = f.to_bytes();
        assert_eq!((b[0], b[16], b[24], b[29], b[27]), (7, 4, 7, 8, EMPTY));
    }
    fn listed(c: &Character) -> Vec<(u8, f32)> {
        c.item_slots().iter().take_while(|i| !i.is_empty()).map(|i| (i.idx, i.qty)).collect()
    }

    /// The jgcap2 sell sequence: selling a slot out closes the list up.
    #[test]
    fn selling_a_slot_out_moves_the_rest_up() {
        let mut c = pilot();
        for (idx, q) in [(7, 4.0), (175, 41.0), (176, 0.5), (179, 1.0)] {
            c.add_item(3, idx, q);
        }
        assert_eq!(c.take_item(1, 41.0).map(|(i, t)| (i.idx, t)), Some((175, 41.0)));
        assert_eq!(listed(&c), vec![(7, 4.0), (176, 0.5), (179, 1.0)]);
        // Slot 1 is now the next item.
        assert_eq!(c.take_item(1, 0.5).map(|(i, _)| i.idx), Some(176));
        assert_eq!(listed(&c), vec![(7, 4.0), (179, 1.0)]);
        // A partial sale keeps the slot.
        c.take_item(0, 1.0);
        assert_eq!(listed(&c), vec![(7, 3.0), (179, 1.0)]);
        // A purchase goes after the last item.
        c.add_item(3, 200, 2.0);
        assert_eq!(listed(&c), vec![(7, 3.0), (179, 1.0), (200, 2.0)]);
    }

    /// A list saved with holes (older server) is shown and indexed closed up.
    #[test]
    fn holes_from_old_saves_are_closed() {
        let mut c = pilot();
        c.items = vec![Item::NONE, Item { cat: 3, idx: 9, qty: 1.0 }, Item::NONE, Item { cat: 3, idx: 8, qty: 2.0 }];
        assert_eq!(listed(&c), vec![(9, 1.0), (8, 2.0)]);
        assert_eq!(c.take_item(1, 2.0).map(|(i, _)| i.idx), Some(8));
        assert_eq!(listed(&c), vec![(9, 1.0)]);
    }
}
