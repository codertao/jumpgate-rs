//! Server logic: per-player session state and the request → response handlers.
//!
//! Pure (no sockets): the network loop feeds in `(conn, peer, message)` and
//! sends whatever `Out`s come back. That keeps every handler unit-testable.
//!
//! Request → response pairs are from the 2026-11-10 capture
//! (`notes/captures/`). Responses we can't build yet come from [`Templates`],
//! patched with this player's fields. Each is marked in `docs/server.md`.

use crate::accounts::{Accounts, LoginResult};
use crate::feed::Feed;
use crate::proto::{self, SpaceTarget, ACT_DOCK, ACT_JUMP, ACT_LAUNCH};
use crate::ship::ShipView;
use crate::templates::Templates;
use crate::world::World;
use jumpgate_common::character::Character;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;

pub type PlayerId = u32;

/// Wire ids for server-driven ships start here, clear of player ids (1000+).
/// The client only needs them unique among ships in its sector.
pub const SHIP_PID_BASE: u32 = 0x4000_0000;
/// One-shot ids for replies to refused logins (see `Server::rejected`).
pub const REJECTED_BASE: u32 = 0x7000_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Conn {
    State,
    Space,
}

/// A message to send to one player on one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Out {
    pub to: PlayerId,
    pub conn: Conn,
    pub msg: Vec<u8>,
}

pub struct Config {
    /// IPv4 address written into 0x08 so the client's space connection comes here.
    pub advertise: String,
    /// Station a new pilot is provisioned at (7 = Quantar-Core Station).
    /// Returning pilots start at their account's last base.
    pub home_station: u16,
    /// Flat market price and stock for every item (no economy yet).
    /// Equipment is raised to the client's display floor
    /// (`proto::EQUIPMENT_MIN_PRICE`, 201); commodities use it as is.
    pub price: u32,
    pub stock: u32,
    pub motd: (String, String),
}

impl Default for Config {
    fn default() -> Self {
        Config {
            advertise: "127.0.0.1".into(),
            home_station: crate::accounts::START_BASE,
            price: 99,
            stock: 99,
            motd: (
                "Welcome to a Jumpgate preservation server.".into(),
                "This is a fan-run test server, not jumpgate-tri.org.".into(),
            ),
        }
    }
}

pub struct Player {
    pub id: PlayerId,
    pub callsign: String,
    pub loc: u16,
    pub docked: bool,
    pub state_addr: SocketAddr,
    pub space_addr: Option<SocketAddr>,
    /// Latest 176-byte location report, relayed to others in the sector.
    pub last_report: Option<Vec<u8>>,
}

pub struct Server {
    pub cfg: Config,
    pub tpl: Templates,
    pub world: World,
    pub players: BTreeMap<PlayerId, Player>,
    /// Persistent pilots, keyed by callsign. `Player::callsign` is the key.
    pub accounts: Accounts,
    by_state: HashMap<SocketAddr, PlayerId>,
    by_space: HashMap<SocketAddr, PlayerId>,
    next_id: PlayerId,
    /// Unhandled (conn, type) pairs already reported, to log each once.
    pub unhandled: HashSet<(Conn, u8)>,
    /// 0x09 commands and S→C template types already reported missing.
    unhandled_cmds: HashSet<u8>,
    missing_tpl: HashSet<u8>,
    /// Players who have been sent the sector's existing ships since arriving.
    seen_in_sector: HashSet<PlayerId>,
    /// What each player has been shown of the server-driven ships.
    feed: Feed,
    /// Next spawn# for `!ai` (the bot id in botloc and 0x44).
    next_spawn: u16,
    /// Refused logins awaiting their reply: one-shot id → state address.
    /// Cleared by `tick`, after the network loop has sent the replies.
    rejected: HashMap<PlayerId, SocketAddr>,
    /// Transport sessions to close (a newer login replaced them). Drained by
    /// the network loop with `take_kicks`.
    kicks: Vec<(Conn, SocketAddr)>,
    /// xorshift state for launch-tube choice.
    rng: u64,
    pub log: Vec<String>,
}

/// Docked station id ↔ sector id: the core stations and depots are n and n+100
/// (reversing-map "Location ids"; capture: station 18 launches into 118).
/// 0x08 sub for arriving at a station sector's launch point (as on launch).
pub const LAUNCH_SLOT: u8 = 6;

pub fn sector_of_station(station: u16) -> u16 {
    if station < 100 {
        station + 100
    } else {
        station
    }
}
pub fn station_of_sector(sector: u16) -> Option<u16> {
    (101..=118).contains(&sector).then(|| sector - 100)
}

impl Server {
    pub fn new(cfg: Config, tpl: Templates, world: World) -> Self {
        Server {
            cfg,
            tpl,
            world,
            players: BTreeMap::new(),
            accounts: Accounts::in_memory(),
            by_state: HashMap::new(),
            by_space: HashMap::new(),
            next_id: 1000,
            unhandled: HashSet::new(),
            unhandled_cmds: HashSet::new(),
            missing_tpl: HashSet::new(),
            seen_in_sector: HashSet::new(),
            feed: Feed::new(),
            next_spawn: 0,
            rejected: HashMap::new(),
            kicks: Vec::new(),
            rng: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64)
                | 1,
            log: Vec::new(),
        }
    }

    /// Sessions the network loop should close (sends RakNet's disconnect).
    pub fn take_kicks(&mut self) -> Vec<(Conn, SocketAddr)> {
        std::mem::take(&mut self.kicks)
    }

    fn random(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// A launch tube for a launch into `sector`: one of the station model's
    /// tubes at random (the classic server chose; captures show 0 and 2).
    fn pick_tube(&mut self, sector: u16) -> u8 {
        let tubes = self.world.station(sector).map(|(_, sp)| sp.launch_tubes()).unwrap_or_default();
        if tubes.is_empty() {
            return 0;
        }
        let i = (self.random() % tubes.len() as u64) as usize;
        tubes[i]
    }

    /// One frame of server-driven ships (from the NPC sim, or anything else):
    /// `(id, ship)` pairs, ids local to the caller. Shown to players in space who
    /// have reported from their sector, i.e. whose client has finished arriving.
    pub fn present_ships<'a>(&mut self, ships: impl IntoIterator<Item = (u32, &'a ShipView)>, clock: f32) -> Vec<Out> {
        let viewers: Vec<(PlayerId, u16)> = self
            .players
            .values()
            .filter(|p| !p.docked && p.space_addr.is_some() && self.seen_in_sector.contains(&p.id))
            .map(|p| (p.id, p.loc))
            .collect();
        let ships: Vec<(u32, &ShipView)> = ships.into_iter().map(|(id, s)| (SHIP_PID_BASE + id, s)).collect();
        self.feed
            .update(&viewers, &ships, clock)
            .into_iter()
            .map(|(to, msg)| Out { to, conn: Conn::State, msg })
            .collect()
    }

    fn note(&mut self, s: String) {
        eprintln!("[game] {s}");
        self.log.push(s);
    }

    /// A transport-level session went away.
    pub fn disconnected(&mut self, conn: Conn, addr: SocketAddr) -> Vec<Out> {
        match conn {
            Conn::State => match self.by_state.get(&addr).copied() {
                Some(pid) => self.logout(pid),
                None => Vec::new(),
            },
            Conn::Space => {
                let Some(pid) = self.by_space.remove(&addr) else { return Vec::new() };
                // Only the player's current space session counts. An older one
                // (left over from before a dock and relaunch) timing out must
                // not unbind the new one: that left the client's space
                // keepalives unanswered until it gave up ("Disconnected from
                // space server", 2026-11-19).
                if self.players.get(&pid).and_then(|p| p.space_addr) != Some(addr) {
                    self.note(format!("player {pid}: old space session {addr} closed"));
                    return Vec::new();
                }
                let mut out = self.despawn(pid);
                if let Some(p) = self.players.get_mut(&pid) {
                    p.space_addr = None;
                }
                out.retain(|o| o.to != pid);
                out
            }
        }
    }

    /// A space session finished its handshake: bind it to the player on the same
    /// host who is in space and has no space session yet (latest login wins).
    pub fn space_connected(&mut self, addr: SocketAddr) {
        let ip = addr.ip();
        let pid = self
            .players
            .values()
            .rev()
            .find(|p| p.state_addr.ip() == ip && p.space_addr.is_none() && !p.docked)
            .map(|p| p.id);
        if let Some(pid) = pid {
            self.bind_space(pid, addr);
        }
    }

    fn bind_space(&mut self, pid: PlayerId, addr: SocketAddr) {
        // A player has one space session: forget the previous one's address.
        if let Some(prev) = self.players.get(&pid).and_then(|p| p.space_addr).filter(|a| *a != addr) {
            self.by_space.remove(&prev);
        }
        if let Some(old) = self.by_space.insert(addr, pid) {
            if old != pid {
                if let Some(p) = self.players.get_mut(&old) {
                    p.space_addr = None;
                }
            }
        }
        if let Some(p) = self.players.get_mut(&pid) {
            p.space_addr = Some(addr);
            let cs = p.callsign.clone();
            self.note(format!("{cs} ({pid}) space session {addr}"));
        }
    }

    pub fn handle(&mut self, conn: Conn, addr: SocketAddr, msg: &[u8]) -> Vec<Out> {
        let Some(&ty) = msg.first() else { return Vec::new() };

        // Login is the only message accepted from an unknown state peer.
        if conn == Conn::State && ty == 0x05 && msg.len() <= proto::LOGIN_MAX_LEN {
            return self.login(addr, msg);
        }

        let pid = match conn {
            Conn::State => self.by_state.get(&addr).copied(),
            Conn::Space => self.space_player(addr, msg),
        };
        let Some(pid) = pid else {
            if ty == 0x8f {
                return Vec::new(); // keepalive from a not-yet-bound peer
            }
            self.note(format!("{conn:?} 0x{ty:02x} from unbound peer {addr}; ignored"));
            return Vec::new();
        };

        match (conn, ty) {
            (_, 0x8f) => vec![Out { to: pid, conn, msg: vec![0x8f] }],
            (Conn::State, 0x09) => self.command(pid, msg),
            (Conn::State, 0xc1) => self.reply_tpl(pid, &[0xc2]),
            (Conn::State, 0x65) => self.reply_tpl(pid, &[0x65]),
            (Conn::State, 0x04) => self.chat(pid, msg),
            (Conn::State, 0x6a) => Vec::new(), // player list request: no reply seen
            (Conn::State, 0x63) => self.reply_tpl(pid, &[0x62]),
            (Conn::State, 0xc3) => self.reply_many(pid, 0x14, 3),
            (Conn::State, 0x8a) => self.weather(pid),
            (Conn::State, 0x91) => {
                let mut out = self.reply_tpl(pid, &[0x91]);
                out.extend(self.reply_many(pid, 0x76, 2));
                out
            }
            (Conn::State, 0x4b) => self.reply_tpl(pid, &[0x4b]),
            (Conn::State, 0x17) => self.reply_tpl(pid, &[0x42, 0xb3]),
            (Conn::State, 0x07) => self.travel(pid, msg),
            (Conn::State, 0x66) => self.fitting(pid, msg),
            (Conn::State, 0x08) => self.change_ship(pid, msg),
            (Conn::State, 0xc8) => self.logout(pid),
            (Conn::State, 0x0e) | (Conn::State, 0x0f) | (Conn::State, 0x74) | (Conn::State, 0x75) => {
                Vec::new() // consumables / building / asteroid: no reply seen
            }
            (Conn::Space, 0x05) => self.location_report(pid, msg),
            (Conn::Space, 0x33) | (Conn::Space, 0x34) => self.relay_fast(pid, msg),
            (Conn::Space, 0x71) => self.relay_botloc(pid, msg),
            (Conn::Space, 0x2c) => Vec::new(), // bot handoff request: not yet
            (_, 0x16) if msg.len() >= 4 => {
                // RejectSpawn: our 0x70 bounced (queue or pbotloc full, or a sector change).
                let (t, n) = (msg[1], proto::u16_at(msg, 2));
                self.note(format!("player {pid} rejected bot spawn ai_type {t} (ship {}) #{n}", t as u16 + 219));
                Vec::new()
            }
            (_, 0x44) if msg.len() >= 12 => {
                let (n, killer) = (proto::u16_at(msg, 2), proto::u32_at(msg, 4));
                self.note(format!("player {pid}: bot #{n} destroyed by {killer}"));
                Vec::new()
            }
            _ => {
                if self.unhandled.insert((conn, ty)) {
                    self.note(format!("unhandled {conn:?} 0x{ty:02x} ({}B) — first seen", msg.len()));
                }
                Vec::new()
            }
        }
    }

    /// Resolve a space peer. A location report names its player (@0x5c); accept
    /// that only if the player's state session is on the same host.
    fn space_player(&mut self, addr: SocketAddr, msg: &[u8]) -> Option<PlayerId> {
        if msg.first() == Some(&0x05) && msg.len() >= proto::LOC_PID_OFF + 4 {
            let claimed = proto::u32_at(msg, proto::LOC_PID_OFF);
            let same_host = self
                .players
                .get(&claimed)
                .is_some_and(|p| p.state_addr.ip() == addr.ip());
            if same_host && self.by_space.get(&addr) != Some(&claimed) {
                self.bind_space(claimed, addr);
            }
        }
        self.by_space.get(&addr).copied()
    }

    // ---- handlers ----

    fn login(&mut self, addr: SocketAddr, msg: &[u8]) -> Vec<Out> {
        let Some(login) = proto::parse_login(msg) else {
            self.note(format!("malformed login from {addr}"));
            return Vec::new();
        };
        // The password is only hashed (new account) or verified, then dropped.
        // Never log it (CLAUDE.md "Credentials").
        let result = self.accounts.login(&login.callsign, &login.password, self.cfg.home_station);
        drop(login.password);
        if let LoginResult::WrongPassword = result {
            self.note(format!("login {} from {addr}: wrong password", login.callsign));
            // No player exists: address the reply by a one-shot id.
            let to = REJECTED_BASE + (self.rejected.len() as u32);
            self.rejected.insert(to, addr);
            let text = "Wrong password for that callsign.";
            return vec![Out { to, conn: Conn::State, msg: proto::system_chat(0, "Server", text) }];
        }
        // Whatever a replaced session leaves behind: its ship's removal for
        // the sector (sent with this login's replies) and its connections.
        let mut gone = Vec::new();
        if let Some(old) = self.by_state.get(&addr).copied() {
            gone.extend(self.logout(old));
        }
        // One session per account: a new login replaces the old one and
        // closes its connections, so a stale client can't keep flying it.
        let key = login.callsign.to_ascii_lowercase();
        if let Some(old) = self.players.values().find(|p| p.callsign.to_ascii_lowercase() == key).map(|p| p.id) {
            self.note(format!("{} logged in again; dropping session {old}", login.callsign));
            let (old_state, old_space) = (self.players[&old].state_addr, self.players[&old].space_addr);
            gone.extend(self.logout(old));
            if old_state != addr {
                self.kicks.push((Conn::State, old_state));
            }
            if let Some(a) = old_space {
                self.kicks.push((Conn::Space, a));
            }
        }
        let acct = self.accounts.get(&login.callsign).expect("account exists after login");
        let callsign = acct.pilot.callsign.clone();
        let loc = acct.last_base;
        let pid = self.next_id;
        self.next_id += 1;
        if let Some(a) = self.accounts.get_mut(&callsign) {
            a.loc = loc;
        }
        self.players.insert(
            pid,
            Player {
                id: pid,
                callsign: callsign.clone(),
                loc,
                docked: true,
                state_addr: addr,
                space_addr: None,
                last_report: None,
            },
        );
        self.by_state.insert(addr, pid);
        let new = matches!(result, LoginResult::Provisioned);
        self.note(format!(
            "login {callsign} → player {pid} at station {loc} (client {}){}",
            login.version,
            if new { ", new account" } else { "" }
        ));

        let mut out = vec![self.char_record(pid)];
        out.extend(self.reply_tpl(pid, &[0x8c]));
        let (a, b) = self.cfg.motd.clone();
        out.push(Out { to: pid, conn: Conn::State, msg: proto::motd(&a, &b) });
        out.extend(self.reply_tpl(pid, &[0x18]));
        if new {
            let text = "New pilot created. This server stores only a salted hash of your password, \
                        but the game sends it unencrypted: don't reuse a password from anywhere else.";
            out.push(Out { to: pid, conn: Conn::State, msg: proto::system_chat(loc, "Server", text) });
        }
        out.extend(gone);
        out
    }

    /// The pilot behind a session (every logged-in player has one).
    fn pilot(&self, pid: PlayerId) -> &Character {
        &self.accounts.get(&self.players[&pid].callsign).expect("account").pilot
    }

    fn pilot_mut(&mut self, pid: PlayerId) -> &mut Character {
        let cs = &self.players[&pid].callsign;
        &mut self.accounts.get_mut(cs).expect("account").pilot
    }

    /// Record where a player is, and their last base when docked.
    fn set_account_loc(&mut self, pid: PlayerId, loc: u16, docked: bool) {
        let cs = self.players[&pid].callsign.clone();
        if let Some(a) = self.accounts.get_mut(&cs) {
            a.loc = loc;
            if docked {
                a.last_base = loc;
            }
        }
    }

    fn char_record(&self, pid: PlayerId) -> Out {
        let loc = self.players[&pid].loc;
        let rec = proto::char_record(self.tpl.first(0x06), self.pilot(pid), pid, loc);
        Out { to: pid, conn: Conn::State, msg: rec }
    }

    fn credits_items(&self, pid: PlayerId) -> Out {
        Out { to: pid, conn: Conn::State, msg: proto::credits_items(self.pilot(pid)) }
    }

    fn experience(&self, pid: PlayerId) -> Out {
        Out { to: pid, conn: Conn::State, msg: proto::experience(self.pilot(pid).experience) }
    }

    fn tell(&self, pid: PlayerId, text: &str) -> Out {
        Out { to: pid, conn: Conn::State, msg: proto::system_chat(self.players[&pid].loc, "Server", text) }
    }

    /// Periodic work: the account snapshot (once a minute when changed).
    pub fn tick(&mut self, now: std::time::Instant) {
        self.rejected.clear();
        if let Err(e) = self.accounts.save_if_due(now) {
            self.note(format!("account snapshot failed: {e}"));
        }
    }

    /// 0x09 station market: listings (flat price/stock), buy and sell. Buys
    /// and sells answer with the updated 0x45.
    fn command(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
        let cmd = msg.get(1).copied().unwrap_or(0xff);
        if let Some(cat) = proto::listing_category(cmd) {
            let m = proto::listing(cat, self.cfg.stock, self.cfg.price).expect("listing category");
            return vec![Out { to: pid, conn: Conn::State, msg: m }];
        }
        let Some(mc) = proto::parse_market_cmd(msg) else { return Vec::new() };
        if let Some(cat) = proto::buy_category(cmd) {
            return self.buy(pid, cat, mc.index, mc.qty);
        }
        if cmd == proto::CMD_SELL {
            return self.sell(pid, mc.index as usize, mc.qty);
        }
        if self.unhandled_cmds.insert(cmd) {
            self.note(format!("0x09 command 0x{cmd:02x} not handled yet"));
        }
        Vec::new()
    }

    fn valid_qty(&self, qty: f32) -> bool {
        qty.is_finite() && qty > 0.0 && qty <= self.cfg.stock as f32
    }

    /// Unit price of a category, as its listing shows it.
    fn price(&self, cat: u8) -> u32 {
        proto::listed_price(cat, self.cfg.price)
    }

    fn cost(&self, cat: u8, qty: f32) -> u64 {
        (qty * self.price(cat) as f32).ceil() as u64
    }

    fn buy(&mut self, pid: PlayerId, cat: u8, index: u32, qty: f32) -> Vec<Out> {
        let in_range = proto::listing_for_category(cat).is_some_and(|(_, n)| (index as usize) < n);
        if !self.players[&pid].docked || !in_range || !self.valid_qty(qty) {
            self.note(format!("player {pid}: bad buy cat {cat} #{index} ×{qty}"));
            return vec![self.credits_items(pid)];
        }
        let cost = self.cost(cat, qty);
        let c = self.pilot_mut(pid);
        if c.credits < cost {
            let out = self.tell(pid, &format!("You need {cost} credits for that."));
            return vec![out, self.credits_items(pid)];
        }
        if !c.add_item(cat, index as u8, qty) {
            let out = self.tell(pid, "Your hold has no free slot.");
            return vec![out, self.credits_items(pid)];
        }
        c.credits -= cost;
        self.note(format!("player {pid} bought cat {cat} #{index} ×{qty} for {cost}"));
        vec![self.credits_items(pid)]
    }

    fn sell(&mut self, pid: PlayerId, slot: usize, qty: f32) -> Vec<Out> {
        if !self.players[&pid].docked || !self.valid_qty(qty) {
            return vec![self.credits_items(pid)];
        }
        let Some(cat) = self.pilot(pid).items.get(slot).map(|i| i.cat) else {
            return vec![self.credits_items(pid)];
        };
        let price = self.price(cat) as f32;
        let c = self.pilot_mut(pid);
        if let Some((item, took)) = c.take_item(slot, qty) {
            let gain = (took * price).floor() as u64;
            c.credits += gain;
            self.note(format!("player {pid} sold cat {} #{} ×{took} for {gain}", item.cat, item.idx));
        }
        vec![self.credits_items(pid)]
    }

    /// C→S 0x66 fitting change. Op 2 (missile fired, in space) uses up the
    /// rack's missile [inferred: the rack empties; partial racks not modelled].
    fn fitting(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
        let Some(f) = proto::parse_fit_change(msg) else { return Vec::new() };
        let c = self.pilot_mut(pid);
        let ok = match f.op {
            proto::FIT_UNFIT => c.unfit(f.list_slot as usize, f.cat, f.ship_slot),
            proto::FIT_FIT => c.fit(f.list_slot as usize, f.cat, f.ship_slot),
            proto::FIT_MISSILE_FIRED => {
                if let Some(s) = c.fittings.slot_mut(f.cat, f.ship_slot) {
                    *s = jumpgate_common::character::EMPTY;
                }
                return Vec::new();
            }
            _ => false,
        };
        if !ok {
            self.note(format!(
                "player {pid}: fitting op {} slot {} cat {} ship slot {} refused",
                f.op, f.list_slot, f.cat, f.ship_slot
            ));
        }
        vec![self.credits_items(pid)]
    }

    /// C→S 0x08 change ship (docked). The client unfits everything first; any
    /// equipment still fitted goes back to the item list.
    fn change_ship(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
        let Some(ty) = proto::parse_change_ship(msg) else { return Vec::new() };
        if !self.players[&pid].docked || self.world.hull(ty as u16).is_none() && !self.world.hulls.is_empty() {
            self.note(format!("player {pid}: ship change to {ty} refused"));
            return vec![self.credits_items(pid), self.char_record(pid)];
        }
        let cost = self.cfg.price as u64;
        let c = self.pilot_mut(pid);
        if c.credits < cost {
            let out = self.tell(pid, &format!("You need {cost} credits for that ship."));
            return vec![out, self.credits_items(pid), self.char_record(pid)];
        }
        c.credits -= cost;
        let fitted = c.fittings.to_bytes();
        // Category of each fittings byte (layout in `Fittings::to_bytes`).
        let cats = [0u8; 8].into_iter().chain([1; 8]).chain([11; 8]).chain([5, 6, 7, 8, 9, 10]);
        for (cat, idx) in cats.zip(fitted) {
            if idx != jumpgate_common::character::EMPTY {
                c.add_item(cat, idx, 1.0);
            }
        }
        c.fittings = Default::default();
        c.ship_type = ty;
        self.note(format!("player {pid} changed ship to {ty}"));
        vec![self.credits_items(pid), self.char_record(pid)]
    }

    /// Relay a player's chat line to every other logged-in player. [inferred:
    /// global channel, verbatim relay]
    fn chat(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
        // `!` lines are server commands (the client drops unknown /commands
        // itself, slash-commands.md), answered privately and not relayed.
        if let Some(text) = proto::chat_text(msg).filter(|t| t.starts_with('!')) {
            return self.bang_command(pid, &text);
        }
        self.players
            .keys()
            .filter(|&&id| id != pid)
            .map(|&to| Out { to, conn: Conn::State, msg: msg.to_vec() })
            .collect()
    }

    /// 0x8A weather request. Docked: 0x87. In space: 0x61 beacon (open-space
    /// sectors only), 0x73, 0x87 (capture after a jump also had 0x58 and the
    /// 0xA7 station list; not sent yet).
    fn weather(&mut self, pid: PlayerId) -> Vec<Out> {
        let p = &self.players[&pid];
        if p.docked {
            self.reply_tpl(pid, &[0x87])
        } else {
            let loc = p.loc;
            let mut out: Vec<Out> = proto::beacon_update(loc, 0)
                .map(|msg| Out { to: pid, conn: Conn::State, msg })
                .into_iter()
                .collect();
            out.extend(self.reply_tpl(pid, &[0x73, 0x87]));
            out
        }
    }

    fn travel(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
        let Some(tr) = proto::parse_travel(msg) else { return Vec::new() };
        let advertise = self.cfg.advertise.clone();
        let cur = self.players[&pid].loc;
        match tr.action {
            ACT_LAUNCH => {
                let sector = sector_of_station(cur);
                let mut out = self.despawn(pid);
                {
                    let p = self.players.get_mut(&pid).unwrap();
                    p.docked = false;
                    p.loc = sector;
                }
                self.set_account_loc(pid, sector, false);
                self.note(format!("player {pid} launch {cur} → sector {sector}"));
                out.push(self.char_record(pid));
                out.extend(self.reply_tpl(pid, &[0x78]));
                let mut m = proto::location_change(
                    self.tpl.launch_redirect.as_deref(),
                    LAUNCH_SLOT,
                    sector,
                    SpaceTarget::Ip(&advertise),
                );
                m[proto::LOCCHG_TUBE_OFF] = self.pick_tube(sector);
                out.push(Out { to: pid, conn: Conn::State, msg: m });
                out
            }
            ACT_JUMP => {
                // 0x07 carries gate number − 1 (reversing-map "Gate links").
                let Some(gate_number) = u8::try_from(tr.arg).ok().and_then(|a| a.checked_add(1)) else {
                    return Vec::new();
                };
                if tr.from_loc != cur {
                    self.note(format!("player {pid}: jump from {} but server has them in {cur}", tr.from_loc));
                }
                let Some(link) = self.world.gate(cur, gate_number) else {
                    self.note(format!("player {pid}: gate {gate_number} in {cur} has no link"));
                    let text = format!("Gate {gate_number} in {} doesn't lead anywhere.", self.world.name(cur));
                    return vec![Out { to: pid, conn: Conn::State, msg: proto::system_chat(cur, "Server", &text) }];
                };
                self.note(format!(
                    "player {pid} jump {} ({cur}) gate {gate_number} → {} ({}) gate {}",
                    self.world.name(cur),
                    self.world.name(link.dest_loc),
                    link.dest_loc,
                    link.dest_gate
                ));
                self.relocate(pid, link.dest_loc, link.dest_gate)
            }
            ACT_DOCK => {
                let station = station_of_sector(cur).unwrap_or(self.cfg.home_station);
                let mut out = self.despawn(pid);
                {
                    // Dock drops the client's space connection (0x08 "none"); the
                    // next launch opens a new one, which space_connected binds.
                    let p = self.players.get_mut(&pid).unwrap();
                    if let Some(a) = p.space_addr.take() {
                        self.by_space.remove(&a);
                    }
                    p.docked = true;
                    p.loc = station;
                    p.last_report = None;
                }
                self.set_account_loc(pid, station, true);
                self.note(format!("player {pid} dock {cur} → station {station}"));
                out.push(self.credits_items(pid));
                out.push(self.experience(pid));
                if let Some(t) = self.tpl.first(0xba) {
                    let mut m = t.to_vec();
                    if m.len() >= 8 {
                        proto::put_u32(&mut m, 4, pid);
                    }
                    out.push(Out { to: pid, conn: Conn::State, msg: m });
                }
                out.extend(self.reply_tpl(pid, &[0x4b]));
                out.push(self.char_record(pid));
                out.extend(self.reply_tpl(pid, &[0x8c]));
                let m = proto::location_change(None, 0xcc, station, SpaceTarget::None);
                out.push(Out { to: pid, conn: Conn::State, msg: m });
                out
            }
            other => {
                self.note(format!("player {pid}: 0x07 action {other} not handled"));
                Vec::new()
            }
        }
    }

    /// Move a player in space to `dest`, arriving at contact slot `sub` (a gate
    /// number 1–4, or 6 = a station's launch point): the same 0x78 + 0x08 a
    /// gate jump gets.
    fn relocate(&mut self, pid: PlayerId, dest: u16, sub: u8) -> Vec<Out> {
        let advertise = self.cfg.advertise.clone();
        let mut out = self.despawn(pid);
        self.players.get_mut(&pid).unwrap().loc = dest;
        self.set_account_loc(pid, dest, false);
        out.extend(self.reply_tpl(pid, &[0x78]));
        // sub = arrival slot: the client spawns the ship at that contact slot.
        let mut m = proto::location_change(None, sub, dest, SpaceTarget::Ip(&advertise));
        if sub == LAUNCH_SLOT {
            m[proto::LOCCHG_TUBE_OFF] = self.pick_tube(dest);
        }
        out.push(Out { to: pid, conn: Conn::State, msg: m });
        out
    }

    /// `!command` chat lines. Dev/test helpers for now; gate them behind GM
    /// rights once accounts exist.
    fn bang_command(&mut self, pid: PlayerId, text: &str) -> Vec<Out> {
        let mut words = text.split_whitespace();
        let cmd = words.next().unwrap_or("").to_ascii_lowercase();
        let args: Vec<&str> = words.collect();
        let loc = self.players[&pid].loc;
        let reply = |t: String| vec![Out { to: pid, conn: Conn::State, msg: proto::system_chat(loc, "Server", &t) }];
        match cmd.as_str() {
            "!goto" => match self.parse_goto(pid, &args) {
                Ok((dest, sub)) => {
                    let what = if sub == LAUNCH_SLOT { "launch point".to_string() } else { format!("gate {sub}") };
                    self.note(format!("player {pid} !goto {} ({dest}) {what}", self.world.name(dest)));
                    self.relocate(pid, dest, sub)
                }
                Err(e) => reply(e),
            },
            "!ai" => match self.parse_ai(pid, &args) {
                Ok(ship) => {
                    let n = self.next_spawn;
                    self.next_spawn = self.next_spawn.wrapping_add(1);
                    let name = self.world.hulls.get(&ship).map_or("?".to_string(), |h| h.name.clone());
                    self.note(format!("player {pid} !ai {name} (ship {ship}) #{n} in {loc}"));
                    let m = proto::bot_spawn(ship, n).expect("parse_ai returns bot types");
                    let mut out = reply(format!("Spawning {name} (#{n}) in about 5 s."));
                    out.insert(0, Out { to: pid, conn: Conn::State, msg: m });
                    out
                }
                Err(e) => reply(e),
            },
            "!help" => reply(
                "!goto <sector id or name> [gate 1-4]: jump straight there (in space only). \
                 !ai <0-27 | jelly | def [1|2] [sol|oct|quan] | 220-257>: spawn a conflux (C-number) or defender, owned by you."
                    .into(),
            ),
            _ => reply(format!("Unknown command {cmd}. Try !help.")),
        }
    }

    /// `!ai <what>` → bot ship type. `<n>` 0–27 is the conflux C-number (C0 = Jellyfish = 230,
    /// C1 'Squid' = 231 … C27 'Great Prawn' = 257), `jelly` = 230, a raw ship
    /// type 220–257 is taken as is. `def [1|2] [sol|oct|quan]` is a faction
    /// defender (220–225), by default for the sector's faction. Turrets (258,
    /// 259) are left out: their spawn# must name a player structure.
    fn parse_ai(&self, pid: PlayerId, args: &[&str]) -> Result<u16, String> {
        const USAGE: &str = "Usage: !ai <0-27 | jelly | def [1|2] [sol|oct|quan] | 220-257>";
        let p = &self.players[&pid];
        if p.docked {
            return Err("Launch first: !ai works in space.".into());
        }
        let loc = p.loc;
        let words: Vec<String> = args.iter().map(|a| a.to_ascii_lowercase()).collect();
        let Some(first) = words.first() else { return Err(USAGE.into()) };
        match first.as_str() {
            "def" | "defender" => {
                // spawn_bot refuses ai_type < 11 (defenders) in locations > 200.
                if loc > 200 {
                    return Err("Defenders only spawn in station sectors (101-200); the client refuses them here.".into());
                }
                let mut level = 1u16;
                let mut faction = self.world.factions.get(&loc).copied().filter(|f| (1..=3).contains(f));
                for w in &words[1..] {
                    match w.as_str() {
                        "1" | "2" => level = w.parse().unwrap(),
                        "sol" | "solrain" => faction = Some(1),
                        "oct" | "octavius" => faction = Some(2),
                        "quan" | "quantar" => faction = Some(3),
                        _ => return Err(USAGE.into()),
                    }
                }
                let f = faction.ok_or_else(|| "This sector has no Solrain, Octavius or Quantar faction; add sol, oct or quan.".to_string())?;
                Ok(220 + 2 * (f as u16 - 1) + (level - 1))
            }
            "jelly" | "jellyfish" => Ok(230),
            w => {
                let n: u16 = w.trim_start_matches('c').parse().map_err(|_| USAGE.to_string())?;
                match n {
                    0..=27 => Ok(230 + n),
                    220..=225 if loc > 200 => {
                        Err("Defenders only spawn in station sectors (101-200); the client refuses them here.".into())
                    }
                    220..=225 | 230..=257 => Ok(n),
                    _ => Err(format!("No conflux C{n}: they run C0 (Jellyfish) to C27, ships 230-257.")),
                }
            }
        }
    }

    /// `!goto <sector> [gate]`. Sector by id or name (case-insensitive; exact
    /// match first, else a unique substring). Without a gate: a station's launch
    /// point in sectors 101–118, otherwise the lowest-numbered gate.
    fn parse_goto(&self, pid: PlayerId, args: &[&str]) -> Result<(u16, u8), String> {
        if self.players[&pid].docked {
            return Err("Launch first: !goto works in space.".into());
        }
        if args.is_empty() {
            return Err("Usage: !goto <sector id or name> [gate 1-4]".into());
        }
        let (name_args, gate) = match args.last().and_then(|a| a.parse::<u8>().ok()) {
            Some(g) if args.len() > 1 && (1..=4).contains(&g) => (&args[..args.len() - 1], Some(g)),
            _ => (args, None),
        };
        let query = name_args.join(" ");
        let is_sector = |id: u16| (101..=999).contains(&id);
        let dest = if let Ok(id) = query.parse::<u16>() {
            id
        } else {
            // Compare letters and digits only: "quantar core" finds "Quantar-Core Station".
            let norm = |t: &str| t.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
            let q = norm(&query);
            let named = |exact: bool| -> Vec<u16> {
                let mut v: Vec<u16> = self
                    .world
                    .names
                    .iter()
                    .filter(|(id, n)| {
                        let n = norm(n);
                        is_sector(**id) && if exact { n == q } else { n.contains(&q) }
                    })
                    .map(|(id, _)| *id)
                    .collect();
                v.sort();
                v
            };
            match (named(true).as_slice(), named(false).as_slice()) {
                ([id], _) | ([], [id]) => *id,
                ([], []) => return Err(format!("No sector matches {query:?}.")),
                (_, many) => {
                    let list: Vec<String> = many.iter().take(6).map(|id| format!("{} ({id})", self.world.name(*id))).collect();
                    return Err(format!("{query:?} is ambiguous: {}", list.join(", ")));
                }
            }
        };
        if !is_sector(dest) || !self.world.names.contains_key(&dest) {
            return Err(format!("{dest} isn't a sector."));
        }
        let gates: Vec<u8> = (1..=4).filter(|g| self.world.gates.contains_key(&(dest, *g))).collect();
        let sub = match gate {
            Some(g) if gates.contains(&g) => g,
            Some(g) => return Err(format!("{} ({dest}) has no gate {g}; gates: {gates:?}", self.world.name(dest))),
            None if station_of_sector(dest).is_some() => LAUNCH_SLOT,
            None => *gates.first().ok_or(format!("{} ({dest}) has no gates to arrive at.", self.world.name(dest)))?,
        };
        Ok((dest, sub))
    }

    /// 176-byte report: remember it and relay to others in the same sector,
    /// which spawns/updates the sender's ship on their screens.
    fn location_report(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
        if msg.len() != proto::LOC_REPORT_LEN {
            return Vec::new();
        }
        let mut m = msg.to_vec();
        // The server is authoritative for identity and location.
        proto::put_u32(&mut m, proto::LOC_PID_OFF, pid);
        let loc = self.players[&pid].loc;
        proto::put_u16(&mut m, proto::LOC_LOC_OFF, loc);
        self.players.get_mut(&pid).unwrap().last_report = Some(m.clone());

        let mut out = self.to_sector(pid, Conn::State, &m);
        // A newcomer also needs everyone already here.
        let others: Vec<(PlayerId, Vec<u8>)> = self
            .sector_mates(pid)
            .filter_map(|p| p.last_report.clone().map(|r| (p.id, r)))
            .collect();
        let first_report = !self.seen_in_sector.contains(&pid);
        if first_report {
            for (_, r) in others {
                out.push(Out { to: pid, conn: Conn::State, msg: r });
            }
            self.seen_in_sector.insert(pid);
        }
        out
    }

    fn relay_fast(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
        if proto::fast_loc_pid(msg) != Some(pid) {
            return Vec::new(); // must describe the sender's own ship
        }
        self.to_sector(pid, Conn::State, msg)
    }

    /// C→S 0x71 botloc from a bot's controller → S→C 0x71 (same layout) to the
    /// rest of the sector, which creates or updates the bot there (handler
    /// `0x9d6f25`, ai.md "Spawning"). Dropped unless it is a real bot (ship
    /// type 219–259, or 0xffff which the handler also accepts; the "no bots"
    /// botloc has type 0), the sender is the controller (@0x20), and the loc
    /// (@0xa) is the sender's current sector (the receiving handler drops
    /// other locs anyway).
    fn relay_botloc(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
        let Some(b) = proto::parse_botloc(msg) else { return Vec::new() };
        let loc = self.players[&pid].loc;
        if !b.is_bot() || b.controller != pid || b.loc != loc {
            return Vec::new();
        }
        self.to_sector(pid, Conn::State, msg)
    }

    /// Tell sector-mates this player's ship has left (last report with ship
    /// type set to "gone" [inferred: 0xfa is the removal value in `0x9cf7c8`]).
    fn despawn(&mut self, pid: PlayerId) -> Vec<Out> {
        self.seen_in_sector.remove(&pid);
        let Some(mut r) = self.players.get(&pid).and_then(|p| p.last_report.clone()) else {
            return Vec::new();
        };
        proto::put_u16(&mut r, proto::LOC_SHIP_OFF, proto::SHIP_GONE);
        let out = self.to_sector(pid, Conn::State, &r);
        self.players.get_mut(&pid).unwrap().last_report = None;
        out
    }

    fn logout(&mut self, pid: PlayerId) -> Vec<Out> {
        let out = self.despawn(pid);
        if let Some(p) = self.players.remove(&pid) {
            self.by_state.remove(&p.state_addr);
            if let Some(a) = p.space_addr {
                self.by_space.remove(&a);
            }
            self.note(format!("logout {} ({pid})", p.callsign));
            if let Err(e) = self.accounts.save_now() {
                self.note(format!("account snapshot failed: {e}"));
            }
        }
        out
    }

    // ---- helpers ----

    fn sector_mates(&self, pid: PlayerId) -> impl Iterator<Item = &Player> {
        let me = &self.players[&pid];
        let (loc, docked) = (me.loc, me.docked);
        self.players.values().filter(move |p| p.id != pid && !p.docked && !docked && p.loc == loc)
    }

    fn to_sector(&self, pid: PlayerId, conn: Conn, msg: &[u8]) -> Vec<Out> {
        self.sector_mates(pid).map(|p| Out { to: p.id, conn, msg: msg.to_vec() }).collect()
    }

    /// Send the first captured template of each type (skipped if not loaded).
    fn reply_tpl(&mut self, pid: PlayerId, types: &[u8]) -> Vec<Out> {
        let mut out = Vec::new();
        for &ty in types {
            match self.tpl.first(ty) {
                Some(t) => out.push(Out { to: pid, conn: Conn::State, msg: t.to_vec() }),
                None => {
                    if self.missing_tpl.insert(ty) {
                        self.note(format!("no template for S→C 0x{ty:02x}; not sent"));
                    }
                }
            }
        }
        out
    }

    fn reply_many(&mut self, pid: PlayerId, ty: u8, n: usize) -> Vec<Out> {
        self.tpl.first_n(ty, n).into_iter().map(|msg| Out { to: pid, conn: Conn::State, msg }).collect()
    }

    pub fn state_addr(&self, pid: PlayerId) -> Option<SocketAddr> {
        self.players.get(&pid).map(|p| p.state_addr).or_else(|| self.rejected.get(&pid).copied())
    }
    pub fn space_addr(&self, pid: PlayerId) -> Option<SocketAddr> {
        self.players.get(&pid).and_then(|p| p.space_addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raknet::game as g;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn login_msg(cs: &str) -> Vec<u8> {
        let mut m = vec![0u8; 33];
        m[0] = 0x05;
        m[1..1 + cs.len()].copy_from_slice(cs.as_bytes());
        m[proto::LOGIN_VERSION_OFF..proto::LOGIN_VERSION_OFF + 6].copy_from_slice(b"2.0127");
        m
    }

    fn report(pid: PlayerId, x: u8) -> Vec<u8> {
        let mut m = vec![0u8; proto::LOC_REPORT_LEN];
        m[0] = 0x05;
        proto::put_u16(&mut m, proto::LOC_SHIP_OFF, 0x5c);
        proto::put_u32(&mut m, proto::LOC_PID_OFF, pid);
        m[0x90] = x;
        m
    }

    fn travel(action: u8, loc: u16, arg: u32, pid: PlayerId) -> Vec<u8> {
        let mut m = vec![0x07, action];
        m.extend_from_slice(&loc.to_le_bytes());
        m.extend_from_slice(&arg.to_le_bytes());
        m.extend_from_slice(&pid.to_le_bytes());
        m
    }

    fn test_world() -> World {
        let mut w = World::default();
        w.gates.insert((118, 2), crate::world::GateLink { dest_loc: 241, dest_gate: 2 });
        w
    }

    /// Tests start new pilots at Quantar Depot (18 → sector 118), where the
    /// capture was taken.
    fn server() -> Server {
        Server::new(Config { home_station: 18, ..Config::default() }, Templates::default(), test_world())
    }

    /// Log in and launch; returns the player id.
    fn into_space(s: &mut Server, state: &str, space: &str) -> PlayerId {
        let cs = format!("pilot{}", s.players.len());
        let out = s.handle(Conn::State, addr(state), &login_msg(&cs));
        let pid = out[0].to;
        s.handle(Conn::State, addr(state), &travel(proto::ACT_LAUNCH, 18, 0, pid));
        s.space_connected(addr(space));
        pid
    }

    #[test]
    fn login_sends_char_record_and_our_motd() {
        let mut s = server();
        let out = s.handle(Conn::State, addr("10.0.0.5:4000"), &login_msg("pilot"));
        let rec = &out[0].msg;
        assert_eq!(rec[0], 0x06);
        assert_eq!(&rec[3..9], b"pilot\0");
        assert_eq!(proto::u32_at(rec, proto::CHAR_PID_OFF), out[0].to);
        assert_eq!(proto::u16_at(rec, proto::CHAR_LOC_OFF), 18);
        let motd = out.iter().find(|o| o.msg[0] == 0x81).unwrap();
        assert!(motd.msg[1..].starts_with(b"Welcome to a Jumpgate preservation server."));
    }

    #[test]
    fn launch_points_space_at_us_and_dock_says_none() {
        let mut s = server();
        let st = addr("10.0.0.5:4000");
        let pid = s.handle(Conn::State, st, &login_msg("pilot"))[0].to;
        let out = s.handle(Conn::State, st, &travel(proto::ACT_LAUNCH, 18, 0, pid));
        let redirect = out.iter().find(|o| o.msg[0] == 0x08).unwrap();
        assert_eq!(g::read_wstr(&redirect.msg, 0x0c, 32).unwrap(), "127.0.0.1");
        assert_eq!(proto::u16_at(&redirect.msg, proto::LOCCHG_LOC_OFF), 118);
        assert!(!s.players[&pid].docked);

        let out = s.handle(Conn::State, st, &travel(proto::ACT_DOCK, 118, 0, pid));
        let redirect = out.iter().find(|o| o.msg[0] == 0x08).unwrap();
        assert!(g::is_space_ip_none(&redirect.msg, 0x0c));
        assert_eq!(proto::u16_at(&redirect.msg, proto::LOCCHG_LOC_OFF), 18);
        assert!(s.players[&pid].docked);
    }

    /// Restarting a client (or agent) mid-flight: the new login must remove the
    /// old ship from everyone's view and close the old connections.
    #[test]
    fn relogin_removes_the_old_ship_and_closes_its_sessions() {
        let mut s = server();
        let a = into_space(&mut s, "10.0.0.5:4000", "10.0.0.5:4001");
        let b = into_space(&mut s, "10.0.0.6:4000", "10.0.0.6:4001");
        s.handle(Conn::Space, addr("10.0.0.5:4001"), &report(a, 1));
        s.handle(Conn::Space, addr("10.0.0.6:4001"), &report(b, 2));
        assert!(s.take_kicks().is_empty());

        // pilot0 (a) logs in again from a new process.
        let out = s.handle(Conn::State, addr("10.0.0.5:5000"), &login_msg("pilot0"));
        let new = out[0].to;
        assert_ne!(new, a);
        let gone = out.iter().find(|o| o.to == b && o.msg[0] == 0x05).expect("b told the old ship left");
        assert_eq!(proto::u16_at(&gone.msg, proto::LOC_SHIP_OFF), proto::SHIP_GONE);
        assert!(!s.players.contains_key(&a));
        let mut kicks = s.take_kicks();
        kicks.sort_by_key(|k| k.1);
        assert_eq!(kicks, vec![(Conn::State, addr("10.0.0.5:4000")), (Conn::Space, addr("10.0.0.5:4001"))]);
    }

    #[test]
    fn launch_picks_one_of_the_stations_tubes() {
        let mut s = Server::new(Config { home_station: 18, ..Config::default() }, Templates::default(), real_world());
        let pid = s.handle(Conn::State, addr("10.0.0.5:4000"), &login_msg("pilot"))[0].to;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..40 {
            let out = s.handle(Conn::State, addr("10.0.0.5:4000"), &travel(proto::ACT_LAUNCH, 18, 0, pid));
            let m = &out.iter().find(|o| o.msg[0] == 0x08).unwrap().msg;
            seen.insert(proto::parse_location_change(m).unwrap().tube);
            s.handle(Conn::State, addr("10.0.0.5:4000"), &travel(proto::ACT_DOCK, 118, 0, pid));
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![0, 1, 2, 3], "Quantar Depot has four tubes");
    }

    fn real_world() -> World {
        World::load(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"))).unwrap()
    }

    #[test]
    fn players_in_a_sector_see_each_other() {
        let mut s = server();
        let a = into_space(&mut s, "10.0.0.5:4000", "10.0.0.5:4001");
        let b = into_space(&mut s, "10.0.0.6:4000", "10.0.0.6:4001");

        // A reports first: nobody else has reported yet, so B gets A's ship.
        let out = s.handle(Conn::Space, addr("10.0.0.5:4001"), &report(a, 1));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to, b);
        // B's first report goes to A, and B is caught up with A's ship.
        let out = s.handle(Conn::Space, addr("10.0.0.6:4001"), &report(b, 2));
        assert!(out.iter().any(|o| o.to == a && o.msg[0x90] == 2));
        assert!(out.iter().any(|o| o.to == b && o.msg[0x90] == 1));

        // Fast updates relay only when they describe the sender's own ship.
        let mut fast = vec![0u8; 56];
        fast[0] = 0x33;
        proto::put_u32(&mut fast, 0x18, a);
        let out = s.handle(Conn::Space, addr("10.0.0.5:4001"), &fast);
        assert_eq!(out.iter().map(|o| o.to).collect::<Vec<_>>(), vec![b]);
        proto::put_u32(&mut fast, 0x18, b);
        assert!(s.handle(Conn::Space, addr("10.0.0.5:4001"), &fast).is_empty());

        // A docks: B is told A's ship is gone.
        let out = s.handle(Conn::State, addr("10.0.0.5:4000"), &travel(proto::ACT_DOCK, 118, 0, a));
        let gone = out.iter().find(|o| o.to == b && o.msg[0] == 0x05).unwrap();
        assert_eq!(proto::u16_at(&gone.msg, proto::LOC_SHIP_OFF), proto::SHIP_GONE);
    }

    #[test]
    fn space_report_cannot_claim_another_hosts_player() {
        let mut s = server();
        let a = into_space(&mut s, "10.0.0.5:4000", "10.0.0.5:4001");
        // A peer on another host claims to be A.
        let out = s.handle(Conn::Space, addr("10.0.0.9:4001"), &report(a, 9));
        assert!(out.is_empty());
        assert_eq!(s.players[&a].space_addr, Some(addr("10.0.0.5:4001")));
    }

    #[test]
    fn jump_uses_gate_links() {
        let mut s = server();
        let a = into_space(&mut s, "10.0.0.5:4000", "10.0.0.5:4001");
        // 0x07 arg 1 = gate 2 → 241, arriving at gate 2.
        let out = s.handle(Conn::State, addr("10.0.0.5:4000"), &travel(proto::ACT_JUMP, 118, 1, a));
        let m = &out.iter().find(|o| o.msg[0] == 0x08).unwrap().msg;
        assert_eq!(proto::u16_at(m, proto::LOCCHG_LOC_OFF), 241);
        assert_eq!(m[1], 2, "sub = arrival gate");
        assert_eq!(g::read_wstr(m, 0x0c, 32).unwrap(), "127.0.0.1");
        assert_eq!(s.players[&a].loc, 241);
    }

    #[test]
    fn unlinked_gate_gets_a_message_not_a_jump() {
        let mut s = server();
        let a = into_space(&mut s, "10.0.0.5:4000", "10.0.0.5:4001");
        let out = s.handle(Conn::State, addr("10.0.0.5:4000"), &travel(proto::ACT_JUMP, 118, 3, a));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].msg[0], 0x04);
        assert_eq!(s.players[&a].loc, 118);
    }

    /// Run the login → launch → jump → dock flow against the real capture, if one
    /// is present locally (it is never in the repo).
    #[test]
    fn flow_against_local_capture() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../jgcap.bin");
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("no local capture at {path}; skipping");
            return;
        };
        let world = World::load(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"))).unwrap();
        let mut s = Server::new(Config { home_station: 18, ..Config::default() }, Templates::from_capture_bytes(&bytes), world);
        let st = addr("10.0.0.5:4000");
        let mut all = s.handle(Conn::State, st, &login_msg("pilot"));
        let pid = all[0].to;
        for cmd in [0x03u8, 0x00, 0x01, 0x60, 0x5f, 0x5e, 0x5d, 0x5c, 0x5b, 0x5a] {
            let out = s.handle(Conn::State, st, &[0x09, cmd, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            assert_eq!(out.len(), 1, "listing for cmd {cmd:#x}");
            assert_eq!(out[0].msg.len(), proto::LISTING_LEN);
            all.extend(out);
        }
        all.extend(s.handle(Conn::State, st, &travel(proto::ACT_LAUNCH, 18, 0, pid)));
        // The capture's first jump: 118, 0x07 arg 1 (gate 2) → 241.
        let out = s.handle(Conn::State, st, &travel(proto::ACT_JUMP, 118, 1, pid));
        assert_eq!(s.players[&pid].loc, 241);
        all.extend(out);
        all.extend(s.handle(Conn::State, st, &travel(proto::ACT_DOCK, 241, 0, pid)));
        for o in &all {
            assert_ne!(o.msg[0], 0xfe, "never send a memory patch");
            if let Some(off) = g::space_ip_offset(o.msg[0]) {
                let ok = g::is_space_ip_none(&o.msg, off)
                    || o.msg[off..off + 2] == [0, 0]
                    || g::read_wstr(&o.msg, off, 32).as_deref() == Some("127.0.0.1");
                assert!(ok, "0x{:02x} names a space server other than us", o.msg[0]);
            }
        }
    }

    /// For every jump in the local capture, the 0x08 built from the extract
    /// equals the captured one except for the space-address field.
    #[test]
    fn built_jump_redirect_matches_capture() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../jgcap.bin");
        let Ok(bytes) = std::fs::read(path) else { return };
        let world = World::load(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"))).unwrap();
        let (records, _) = raknet::capture::parse_lenient(&bytes);
        let mut pending = None;
        let mut checked = 0;
        for r in &records {
            match r.dir {
                raknet::capture::Dir::ClientToServer => {
                    pending = proto::parse_travel(&r.data).filter(|t| t.action == proto::ACT_JUMP);
                }
                raknet::capture::Dir::ServerToClient if r.data[0] == 0x08 => {
                    let Some(t) = pending.take() else { continue };
                    let link = world.gate(t.from_loc, t.arg as u8 + 1).unwrap();
                    let mut built =
                        proto::location_change(None, link.dest_gate, link.dest_loc, SpaceTarget::Ip("127.0.0.1"));
                    let mut captured = r.data.clone();
                    // Compare outside the address field (0x0c..0x2c).
                    built[0x0c..0x2c].fill(0);
                    captured[0x0c..0x2c].fill(0);
                    assert_eq!(built, captured, "jump from {} arg {}", t.from_loc, t.arg);
                    checked += 1;
                }
                _ => {}
            }
        }
        assert_eq!(checked, 3);
    }

    #[test]
    fn npcs_shown_only_after_arrival_in_their_sector() {
        let mut s = server();
        let a = into_space(&mut s, "10.0.0.1:1000", "10.0.0.1:2000");
        let ship = ShipView {
            loc: 118,
            hull_type: 104,
            callsign: "Hauler".into(),
            mass: 515000.0,
            pos: [0.0; 3],
            vel: [0.0; 3],
            fwd: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
            thrust: 0.0,
            armour: 1.0,
            armour_max: 62500.0,
            rank: 0,
        };
        // In space but no report from the sector yet: client may still be loading.
        assert!(s.present_ships([(0, &ship)], 1.0).is_empty());
        s.handle(Conn::Space, addr("10.0.0.1:2000"), &report(a, 1));
        let out = s.present_ships([(0, &ship)], 2.0);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].to, out[0].msg[0]), (a, 0x05));
        assert_eq!(proto::u32_at(&out[0].msg, proto::LOC_PID_OFF), SHIP_PID_BASE);
        // Docking: no more NPC traffic.
        s.handle(Conn::State, addr("10.0.0.1:1000"), &travel(ACT_DOCK, 118, 0, a));
        assert!(s.present_ships([(0, &ship)], 3.0).is_empty());
    }


    fn chat_line(pid: PlayerId, text: &str) -> Vec<u8> {
        let mut m = vec![0u8; proto::CHAT_TEXT_OFF];
        m[0] = 0x04;
        m[1] = 5;
        proto::put_u32(&mut m, 4, pid);
        m.extend_from_slice(text.as_bytes());
        m.push(0);
        m
    }

    fn goto_world() -> World {
        let mut w = test_world();
        w.names.insert(118, "Quantar Depot".into());
        w.names.insert(241, "Quantar Core Gate".into());
        w.names.insert(242, "Quantar Core Approach".into());
        w.gates.insert((241, 2), crate::world::GateLink { dest_loc: 118, dest_gate: 2 });
        w.gates.insert((241, 3), crate::world::GateLink { dest_loc: 249, dest_gate: 3 });
        w
    }

    fn sent_redirect(out: &[Out]) -> Option<(u8, u16)> {
        out.iter().find(|o| o.msg[0] == 0x08).map(|o| (o.msg[1], proto::u16_at(&o.msg, proto::LOCCHG_LOC_OFF)))
    }

    #[test]
    fn goto_moves_a_player_in_space_and_is_not_relayed() {
        let mut s = Server::new(Config { home_station: 18, ..Config::default() }, Templates::default(), goto_world());
        let a = into_space(&mut s, "10.0.0.1:1000", "10.0.0.1:2000");
        let b = into_space(&mut s, "10.0.0.2:1000", "10.0.0.2:2000");
        let st = addr("10.0.0.1:1000");
        // By name (exact beats substring), default gate = lowest.
        let out = s.handle(Conn::State, st, &chat_line(a, "!goto quantar core gate"));
        assert_eq!(sent_redirect(&out), Some((2, 241)));
        assert!(out.iter().all(|o| o.to != b), "commands aren't relayed");
        assert_eq!(s.players[&a].loc, 241);
        // By id with a gate.
        let out = s.handle(Conn::State, st, &chat_line(a, "!goto 241 3"));
        assert_eq!(sent_redirect(&out), Some((3, 241)));
        // Station sector without a gate: launch point.
        let out = s.handle(Conn::State, st, &chat_line(a, "!goto Quantar Depot"));
        assert_eq!(sent_redirect(&out), Some((LAUNCH_SLOT, 118)));
        // Punctuation-insensitive substring.
        s.world.names.insert(107, "Quantar-Core Station".into());
        let out = s.handle(Conn::State, st, &chat_line(a, "!goto quantar core station"));
        assert_eq!(sent_redirect(&out), Some((LAUNCH_SLOT, 107)));
    }

    #[test]
    fn goto_errors_are_private_messages() {
        let mut s = Server::new(Config { home_station: 18, ..Config::default() }, Templates::default(), goto_world());
        let a = into_space(&mut s, "10.0.0.1:1000", "10.0.0.1:2000");
        let st = addr("10.0.0.1:1000");
        for bad in ["!goto", "!goto nowhere", "!goto quantar core", "!goto 241 4", "!goto 18", "!bogus"] {
            let out = s.handle(Conn::State, st, &chat_line(a, bad));
            assert_eq!(out.len(), 1, "{bad}");
            assert_eq!((out[0].to, out[0].msg[0]), (a, 0x04), "{bad}");
            assert_eq!(s.players[&a].loc, 118, "{bad} must not move the player");
        }
        // Docked: refused.
        s.handle(Conn::State, st, &travel(ACT_DOCK, 118, 0, a));
        let out = s.handle(Conn::State, st, &chat_line(a, "!goto 241"));
        assert!(sent_redirect(&out).is_none());
    }

    #[test]
    fn ai_spawns_a_bot_for_the_caller() {
        let mut w = goto_world();
        w.factions.insert(118, 3);
        let mut s = Server::new(Config { home_station: 18, ..Config::default() }, Templates::default(), w);
        let a = into_space(&mut s, "10.0.0.1:1000", "10.0.0.1:2000");
        let b = into_space(&mut s, "10.0.0.2:1000", "10.0.0.2:2000");
        let st = addr("10.0.0.1:1000");
        let spawn = |out: &[Out]| out.iter().find(|o| o.msg[0] == 0x70).map(|o| (o.to, o.conn, o.msg.clone()));
        // C3 'Squid' = ship 233 = ai_type 0x0e, as in the capture.
        let out = s.handle(Conn::State, st, &chat_line(a, "!ai 3"));
        assert_eq!(spawn(&out), Some((a, Conn::State, vec![0x70, 0x0e, 0, 0, 0xcc, 0xcc])));
        assert!(out.iter().all(|o| o.to != b));
        // Next spawn# and the C prefix.
        let out = s.handle(Conn::State, st, &chat_line(a, "!ai C27"));
        assert_eq!(spawn(&out).unwrap().2, vec![0x70, 38, 1, 0, 0xcc, 0xcc]);
        assert_eq!(spawn(&s.handle(Conn::State, st, &chat_line(a, "!ai jelly"))).unwrap().2[1], 11);
        assert_eq!(spawn(&s.handle(Conn::State, st, &chat_line(a, "!ai c0"))).unwrap().2[1], 11);
        // Defender: sector faction (Quantar → Quan Def 1 = 224), level and faction overrides.
        assert_eq!(spawn(&s.handle(Conn::State, st, &chat_line(a, "!ai def"))).unwrap().2[1], 224 - 219);
        assert_eq!(spawn(&s.handle(Conn::State, st, &chat_line(a, "!ai def 2 sol"))).unwrap().2[1], 221 - 219);
        // Errors: no 0x70, one private line.
        for bad in ["!ai", "!ai 29", "!ai 258", "!ai def 3", "!ai squid"] {
            let out = s.handle(Conn::State, st, &chat_line(a, bad));
            assert!(spawn(&out).is_none(), "{bad}");
            assert_eq!(out.len(), 1, "{bad}");
        }
        // Defenders are refused outside station sectors (the client would drop them).
        s.handle(Conn::State, st, &chat_line(a, "!goto 241"));
        assert!(spawn(&s.handle(Conn::State, st, &chat_line(a, "!ai def quan"))).is_none());
        assert!(spawn(&s.handle(Conn::State, st, &chat_line(a, "!ai 222"))).is_none());
        assert!(spawn(&s.handle(Conn::State, st, &chat_line(a, "!ai 12"))).is_some());
        // Docked: refused.
        s.handle(Conn::State, st, &travel(ACT_DOCK, 241, 0, a));
        assert!(spawn(&s.handle(Conn::State, st, &chat_line(a, "!ai 3"))).is_none());
    }

    #[test]
    fn botloc_is_relayed_to_the_sector_from_its_controller_only() {
        let mut s = Server::new(Config { home_station: 18, ..Config::default() }, Templates::default(), goto_world());
        let a = into_space(&mut s, "10.0.0.1:1000", "10.0.0.1:2000");
        let b = into_space(&mut s, "10.0.0.2:1000", "10.0.0.2:2000");
        let sp = addr("10.0.0.1:2000");
        s.handle(Conn::Space, sp, &report(a, 1));
        let botloc = |id: u16, loc: u16, ty: u16, ctl: u32| {
            let mut m = vec![0u8; proto::BOTLOC_LEN];
            m[0] = 0x71;
            proto::put_u16(&mut m, 8, id);
            proto::put_u16(&mut m, 0xa, loc);
            proto::put_u16(&mut m, 0xc, ty);
            proto::put_u32(&mut m, 0x20, ctl);
            proto::put_u32(&mut m, 0x24, ctl);
            m
        };
        let m = botloc(0, 118, 233, a);
        let out = s.handle(Conn::Space, sp, &m);
        assert_eq!(out, vec![Out { to: b, conn: Conn::State, msg: m }]);
        // The empty "no bots" report, someone else's bot, a stale loc: dropped.
        assert!(s.handle(Conn::Space, sp, &botloc(1000, 118, 0, a)).is_empty());
        assert!(s.handle(Conn::Space, sp, &botloc(0, 118, 233, b)).is_empty());
        assert!(s.handle(Conn::Space, sp, &botloc(0, 241, 233, a)).is_empty());
        // Not relayed to other sectors.
        s.handle(Conn::State, addr("10.0.0.2:1000"), &chat_line(b, "!goto 241"));
        assert!(s.handle(Conn::Space, sp, &botloc(0, 118, 233, a)).is_empty());
    }

    /// Dock, relaunch on a new space port, then the old space session times
    /// out: the new one must stay bound and keep getting keepalive answers.
    #[test]
    fn stale_space_session_does_not_unbind_the_new_one() {
        let mut s = server();
        let a = into_space(&mut s, "10.0.0.1:1000", "10.0.0.1:2000");
        let (st, old, new) = (addr("10.0.0.1:1000"), addr("10.0.0.1:2000"), addr("10.0.0.1:2001"));
        assert_eq!(s.space_addr(a), Some(old));
        s.handle(Conn::State, st, &travel(ACT_DOCK, 118, 0, a));
        assert_eq!(s.space_addr(a), None, "dock drops space");
        s.handle(Conn::State, st, &travel(proto::ACT_LAUNCH, 18, 0, a));
        s.space_connected(new);
        assert_eq!(s.space_addr(a), Some(new));
        s.disconnected(Conn::Space, old);
        assert_eq!(s.space_addr(a), Some(new));
        let out = s.handle(Conn::Space, new, &[0x8f]);
        assert_eq!(out, vec![Out { to: a, conn: Conn::Space, msg: vec![0x8f] }]);
        // Even if the old address was never forgotten (no dock: e.g. a
        // reconnect from a new port), rebinding drops it.
        s.handle(Conn::Space, addr("10.0.0.1:2002"), &report(a, 1));
        assert_eq!(s.space_addr(a), Some(addr("10.0.0.1:2002")));
        assert!(s.disconnected(Conn::Space, new).is_empty());
        assert_eq!(s.space_addr(a), Some(addr("10.0.0.1:2002")));
    }

    fn login_pw(cs: &str, pw: &str) -> Vec<u8> {
        proto::login(cs, pw)
    }

    fn market(cmd: u8, index: u32, pid: PlayerId, qty: f32) -> Vec<u8> {
        let mut m = vec![0x09, cmd, 7, 0];
        m.extend_from_slice(&index.to_le_bytes());
        m.extend_from_slice(&pid.to_le_bytes());
        m.extend_from_slice(&qty.to_le_bytes());
        m
    }

    fn credits_of(out: &[Out]) -> u64 {
        let m = &out.iter().rev().find(|o| o.msg[0] == 0x45).expect("0x45").msg;
        u64::from_le_bytes(m[8..16].try_into().unwrap())
    }

    #[test]
    fn new_pilot_is_provisioned_at_quantar_core_in_a_storm() {
        let mut s = Server::new(Config::default(), Templates::default(), test_world());
        let out = s.handle(Conn::State, addr("10.0.0.5:4000"), &login_pw("Newbie", "pw"));
        let c = proto::parse_char_record(&out[0].msg).unwrap();
        assert_eq!((c.loc, c.faction, c.ship_type, c.credits), (7, 3, 90, 200_000));
        assert_eq!(c.fittings[24], 0, "starter engine fitted");
        assert!(out.iter().any(|o| o.msg[0] == 0x04), "password-reuse warning");
    }

    #[test]
    fn wrong_password_is_refused_without_a_session() {
        let mut s = server();
        let st = addr("10.0.0.5:4000");
        let pid = s.handle(Conn::State, st, &login_pw("Pilot", "right"))[0].to;
        s.handle(Conn::State, st, &[0xc8]); // logout
        assert!(s.players.is_empty());
        let other = addr("10.0.0.9:4000");
        let out = s.handle(Conn::State, other, &login_pw("pilot", "wrong"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].msg[0], 0x04);
        assert!(s.players.is_empty());
        assert_eq!(s.state_addr(out[0].to), Some(other), "reply reaches the refused peer");
        s.tick(std::time::Instant::now());
        assert_eq!(s.state_addr(out[0].to), None);
        let again = s.handle(Conn::State, st, &login_pw("PILOT", "right"));
        assert_eq!(again[0].msg[0], 0x06);
        assert_ne!(again[0].to, pid, "a fresh session id");
    }

    #[test]
    fn buy_sell_fit_and_change_ship() {
        let mut s = server();
        let st = addr("10.0.0.5:4000");
        let pid = s.handle(Conn::State, st, &login_pw("Trader", "pw"))[0].to;
        // Listings: flat 99/99.
        let out = s.handle(Conn::State, st, &market(0x03, 0, pid, 0.0));
        assert_eq!((out[0].msg[0], proto::u32_at(&out[0].msg, 4), proto::u32_at(&out[0].msg, 8)), (0x0d, 99, 99));
        // Buy 10 of commodity #37: 990 credits.
        let out = s.handle(Conn::State, st, &market(0x07, 37, pid, 10.0));
        assert_eq!(credits_of(&out), 200_000 - 990);
        assert_eq!(&out[0].msg[0x10..0x12], &[3, 37]);
        // Sell 4 back from slot 0.
        let out = s.handle(Conn::State, st, &market(proto::CMD_SELL, 0, pid, 4.0));
        assert_eq!(credits_of(&out), 200_000 - 990 + 396);
        assert_eq!(s.pilot(pid).items[0].qty, 6.0);
        // A full stock of 99 guns #2.
        let out = s.handle(Conn::State, st, &market(0x04, 2, pid, 99.0));
        assert_eq!(credits_of(&out), 200_000 - 990 + 396 - 99 * 201, "equipment at the 201 display floor");
        // Equipment listings show 201 (the client hides anything ≤ 200).
        let gl = s.handle(Conn::State, st, &market(0x00, 0, pid, 0.0));
        assert_eq!((gl[0].msg[0], proto::u32_at(&gl[0].msg, 8)), (0x0a, 201));
        // Buy a gun and fit it in gun slot 0.
        s.handle(Conn::State, st, &market(0x04, 5, pid, 1.0));
        let slot = s.pilot(pid).items.iter().position(|i| i.cat == 0 && i.idx == 5).unwrap() as u8;
        // More than the stock, or more than we can afford: refused.
        let c0 = s.pilot(pid).credits;
        assert_eq!(credits_of(&s.handle(Conn::State, st, &market(0x04, 5, pid, 100.0))), c0);
        s.pilot_mut(pid).credits = 50;
        let out = s.handle(Conn::State, st, &market(0x04, 5, pid, 1.0));
        assert_eq!(credits_of(&out), 50);
        assert!(out.iter().any(|o| o.msg[0] == 0x04), "told why");
        s.pilot_mut(pid).credits = c0;
        s.handle(Conn::State, st, &[0x66, slot, 0, 0, proto::FIT_FIT]);
        assert_eq!(s.pilot(pid).fittings.guns[0], 5);
        // Change ship: anything still fitted goes back to the list.
        let before = s.pilot(pid).credits;
        let out = s.handle(Conn::State, st, &[0x08, 91, 2, 0, 0, 0, 0, 0]);
        let c = proto::parse_char_record(&out.iter().find(|o| o.msg[0] == 0x06).unwrap().msg).unwrap();
        assert_eq!((c.ship_type, c.credits), (91, before - 99));
        assert!(c.fittings.iter().all(|&b| b == 0xfe));
        assert!(s.pilot(pid).items.iter().any(|i| i.cat == 0 && i.idx == 5 && i.qty == 1.0));
        assert!(s.pilot(pid).items.iter().any(|i| i.cat == 5 && i.idx == 0), "starter engine kept");
    }

    #[test]
    fn relogin_starts_at_last_base() {
        let mut s = server();
        s.world.gates.insert((241, 1), crate::world::GateLink { dest_loc: 117, dest_gate: 1 });
        let st = addr("10.0.0.5:4000");
        let pid = s.handle(Conn::State, st, &login_pw("Roamer", "pw"))[0].to;
        s.handle(Conn::State, st, &travel(ACT_LAUNCH, 18, 0, pid));
        s.handle(Conn::State, st, &travel(ACT_JUMP, 118, 1, pid));
        s.handle(Conn::State, st, &travel(ACT_JUMP, 241, 0, pid));
        s.handle(Conn::State, st, &travel(ACT_DOCK, 117, 0, pid));
        s.handle(Conn::State, st, &[0xc8]);
        let a = s.accounts.get("roamer").unwrap();
        assert_eq!((a.loc, a.last_base), (17, 17));
        let out = s.handle(Conn::State, st, &login_pw("Roamer", "pw"));
        assert_eq!(proto::parse_char_record(&out[0].msg).unwrap().loc, 17);
    }
}

