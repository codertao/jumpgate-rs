//! Server logic: per-player session state and the request → response handlers.
//!
//! Pure (no sockets): the network loop feeds in `(conn, peer, message)` and
//! sends whatever `Out`s come back. That keeps every handler unit-testable.
//!
//! Request → response pairs are from the 2026-11-10 capture
//! (`notes/captures/`). Responses we can't build yet come from [`Templates`],
//! patched with this player's fields. Each is marked in `notes/server.md`.

use crate::proto::{self, SpaceTarget, ACT_DOCK, ACT_JUMP, ACT_LAUNCH};
use crate::templates::Templates;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;

pub type PlayerId = u32;

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
    /// Station a new pilot starts docked at (18 = Quantar Depot station).
    pub home_station: u16,
    pub motd: (String, String),
}

impl Default for Config {
    fn default() -> Self {
        Config {
            advertise: "127.0.0.1".into(),
            home_station: 18,
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
    pub players: BTreeMap<PlayerId, Player>,
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
    pub log: Vec<String>,
}

/// Docked station id ↔ sector id: the core stations and depots are n and n+100
/// (reversing-map "Location ids"; capture: station 18 launches into 118).
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
    pub fn new(cfg: Config, tpl: Templates) -> Self {
        Server {
            cfg,
            tpl,
            players: BTreeMap::new(),
            by_state: HashMap::new(),
            by_space: HashMap::new(),
            next_id: 1000,
            unhandled: HashSet::new(),
            unhandled_cmds: HashSet::new(),
            missing_tpl: HashSet::new(),
            seen_in_sector: HashSet::new(),
            log: Vec::new(),
        }
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
                if let Some(pid) = self.by_space.remove(&addr) {
                    let mut out = self.despawn(pid);
                    if let Some(p) = self.players.get_mut(&pid) {
                        p.space_addr = None;
                    }
                    out.retain(|o| o.to != pid);
                    out
                } else {
                    Vec::new()
                }
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
            (Conn::State, 0xc8) => self.logout(pid),
            (Conn::State, 0x0e) | (Conn::State, 0x0f) | (Conn::State, 0x74) | (Conn::State, 0x75) => {
                Vec::new() // consumables / building / asteroid: no reply seen
            }
            (Conn::Space, 0x05) => self.location_report(pid, msg),
            (Conn::Space, 0x33) | (Conn::Space, 0x34) => self.relay_fast(pid, msg),
            (Conn::Space, 0x71) | (Conn::Space, 0x2c) => Vec::new(), // bots: not yet
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
        // No accounts yet: any password is accepted and immediately dropped.
        // When accounts land, check a salted slow hash here (CLAUDE.md).
        drop(login.password);
        if let Some(old) = self.by_state.get(&addr).copied() {
            self.logout(old);
        }
        let pid = self.next_id;
        self.next_id += 1;
        let loc = self.cfg.home_station;
        self.players.insert(
            pid,
            Player {
                id: pid,
                callsign: login.callsign.clone(),
                loc,
                docked: true,
                state_addr: addr,
                space_addr: None,
                last_report: None,
            },
        );
        self.by_state.insert(addr, pid);
        self.note(format!(
            "login {} → player {pid} at station {loc} (client {})",
            login.callsign, login.version
        ));

        let mut out = vec![self.char_record(pid)];
        out.extend(self.reply_tpl(pid, &[0x8c]));
        let (a, b) = self.cfg.motd.clone();
        out.push(Out { to: pid, conn: Conn::State, msg: proto::motd(&a, &b) });
        out.extend(self.reply_tpl(pid, &[0x18]));
        out
    }

    fn char_record(&self, pid: PlayerId) -> Out {
        let p = &self.players[&pid];
        let mut rec = self
            .tpl
            .first(0x06)
            .map(|t| t.to_vec())
            .unwrap_or_else(|| proto::empty_table(0x06, proto::CHAR_LEN));
        proto::patch_char_record(&mut rec, &p.callsign, pid, p.loc);
        Out { to: pid, conn: Conn::State, msg: rec }
    }

    fn command(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
        let cmd = msg.get(1).copied().unwrap_or(0xff);
        match proto::listing_for_command(cmd) {
            Some(ty) => {
                let m = self
                    .tpl
                    .first(ty)
                    .map(|t| t.to_vec())
                    .unwrap_or_else(|| proto::empty_table(ty, proto::LISTING_LEN));
                vec![Out { to: pid, conn: Conn::State, msg: m }]
            }
            None => {
                if self.unhandled_cmds.insert(cmd) {
                    self.note(format!("0x09 command 0x{cmd:02x} not handled yet"));
                }
                Vec::new()
            }
        }
    }

    /// Relay a player's chat line to every other logged-in player. [inferred:
    /// global channel, verbatim relay]
    fn chat(&mut self, pid: PlayerId, msg: &[u8]) -> Vec<Out> {
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
                self.note(format!("player {pid} launch {cur} → sector {sector}"));
                out.push(self.char_record(pid));
                out.extend(self.reply_tpl(pid, &[0x78]));
                let m = proto::location_change(
                    self.tpl.launch_redirect.as_deref(),
                    6,
                    sector,
                    SpaceTarget::Ip(&advertise),
                );
                out.push(Out { to: pid, conn: Conn::State, msg: m });
                out
            }
            ACT_JUMP => {
                let gate = tr.arg;
                let Some(tpl) = self.tpl.gates.get(&(cur, gate)).cloned() else {
                    self.note(format!("player {pid}: gate {gate} in {cur} is not mapped"));
                    let text = format!("Gate {gate} in sector {cur} isn't mapped on this server yet.");
                    return vec![Out { to: pid, conn: Conn::State, msg: proto::system_chat(cur, "Server", &text) }];
                };
                let dest = proto::u16_at(&tpl, proto::LOCCHG_LOC_OFF);
                let mut out = self.despawn(pid);
                self.players.get_mut(&pid).unwrap().loc = dest;
                self.note(format!("player {pid} jump {cur} gate {gate} → {dest}"));
                out.extend(self.reply_tpl(pid, &[0x78]));
                let m = proto::location_change(Some(&tpl), tpl[1], dest, SpaceTarget::Ip(&advertise));
                out.push(Out { to: pid, conn: Conn::State, msg: m });
                out
            }
            ACT_DOCK => {
                let station = station_of_sector(cur).unwrap_or(self.cfg.home_station);
                let mut out = self.despawn(pid);
                {
                    let p = self.players.get_mut(&pid).unwrap();
                    p.docked = true;
                    p.loc = station;
                    p.last_report = None;
                }
                self.note(format!("player {pid} dock {cur} → station {station}"));
                out.extend(self.reply_tpl(pid, &[0x45, 0x92]));
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
        self.players.get(&pid).map(|p| p.state_addr)
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

    fn server() -> Server {
        Server::new(Config::default(), Templates::default())
    }

    /// Log in and launch; returns the player id.
    fn into_space(s: &mut Server, state: &str, space: &str) -> PlayerId {
        let out = s.handle(Conn::State, addr(state), &login_msg("pilot"));
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
    fn unmapped_gate_gets_a_message_not_a_jump() {
        let mut s = server();
        let a = into_space(&mut s, "10.0.0.5:4000", "10.0.0.5:4001");
        let out = s.handle(Conn::State, addr("10.0.0.5:4000"), &travel(proto::ACT_JUMP, 118, 7, a));
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
        let mut s = Server::new(Config::default(), Templates::from_capture_bytes(&bytes));
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
        // The capture's first jump was 118 gate 1 → 241.
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
}
