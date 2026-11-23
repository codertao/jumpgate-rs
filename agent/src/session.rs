//! One agent: a client session over the wire, exactly as the stock client
//! drives it (docs/agents.md). Nothing here touches server internals.
//!
//! Flow: state connect → 0x05 login → 0x06 (pid, ship, loc) → wait docked →
//! 0x07 launch → 0x08 (sub 6, space address) → space connect → 0x05 reports
//! while flying the patrol → 0x0F + 0x07 dock → 0x08 "none" → drop space →
//! wait → launch again.
//!
//! Safety: the space connection always goes to the configured server's host,
//! whatever address the 0x08 names, so an agent can only ever talk to the
//! server it was pointed at.

use crate::ai::{Order, Patrol, Route};
use crate::bots::{self, Bots};
use crate::collide::{self, FieldState, Surroundings};
use crate::missiles::Missiles;
use crate::physics::{Flight, DT};
use crate::sector::Sector;
use jumpgate_common::character::Character;
use jumpgate_common::mesh::Models;
use jumpgate_common::proto::{self, ACT_DOCK, ACT_LAUNCH};
use jumpgate_common::ship::{Hull, Vitals};
use jumpgate_common::world::World;
use raknet::ids::Reliability;
use raknet::session::{Endpoint, Event};
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

const KEEPALIVE_EVERY: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Full 0x05 report: the client sends one every ~5 s while steering.
const FULL_REPORT_EVERY: Duration = Duration::from_secs(5);
/// Fast 0x33/0x34 updates: ~3/s, and only with another ship nearby, as the
/// client does (message-types "Location update path").
const FAST_REPORT_EVERY: Duration = Duration::from_millis(300);
const COMPANY_TIMEOUT: Duration = Duration::from_secs(30);
/// Fallback when the 0x06's engine isn't in the extract.
const DEFAULT_ENGINE_THRUST: f32 = 758_000.0;

#[derive(Clone)]
pub struct Config {
    pub server: IpAddr,
    pub state_port: u16,
    pub space_port: u16,
    pub callsign: String,
    pub password: String,
    pub laps: usize,
    /// Docked pause before (re)launching.
    pub dock_wait: Duration,
    /// Launch/dock cycles before logging out; 0 = forever.
    pub cycles: usize,
    pub verbose: bool,
}

/// One RakNet connection (state or space) on its own UDP socket.
struct Link {
    tag: &'static str,
    sock: UdpSocket,
    ep: Endpoint,
    opened: Instant,
    last_keepalive: Instant,
}

impl Link {
    fn open(tag: &'static str, to: SocketAddr) -> io::Result<Link> {
        let sock = UdpSocket::bind(("0.0.0.0", 0))?;
        sock.connect(to)?;
        sock.set_nonblocking(true)?;
        let mut ep = Endpoint::client();
        ep.set_peer_addr(to);
        for dg in ep.start() {
            sock.send(&dg)?;
        }
        let now = Instant::now();
        Ok(Link { tag, sock, ep, opened: now, last_keepalive: now })
    }

    fn poll(&mut self) -> io::Result<Vec<Event>> {
        let mut buf = [0u8; 2048];
        let mut out = Vec::new();
        loop {
            match self.sock.recv(&mut buf) {
                Ok(n) => out.extend(self.ep.recv(&buf[..n])),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                // ICMP port unreachable on Linux: the server isn't up (yet).
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => break,
                Err(e) => return Err(e),
            }
        }
        if !self.ep.is_connected() && self.opened.elapsed() > CONNECT_TIMEOUT {
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{} connect timed out", self.tag)));
        }
        if self.ep.is_lost() {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, format!("{} connection lost", self.tag)));
        }
        Ok(out)
    }

    fn send(&mut self, msg: &[u8]) {
        self.ep.queue_message(msg);
    }

    fn send_unreliable(&mut self, msg: &[u8]) {
        self.ep.queue_message_rel(msg, Reliability::UnreliableSequenced, 0);
    }

    fn flush(&mut self) {
        if self.ep.is_connected() && self.last_keepalive.elapsed() >= KEEPALIVE_EVERY {
            self.last_keepalive = Instant::now();
            self.ep.queue_message(&proto::KEEPALIVE);
        }
        for dg in self.ep.flush() {
            let _ = self.sock.send(&dg);
        }
    }
}

/// Dropping a link closes it as the stock client does: on dock or redirect
/// its wrapper calls RakPeer::CloseConnection with sendDisconnectionNotification
/// set (`0xa631b0` → vtable +0x58 `0xa6c690` → `0xa6adc0`), so the server sees
/// ID_DISCONNECTION_NOTIFICATION (0x14) at once instead of timing the peer out.
impl Drop for Link {
    fn drop(&mut self) {
        if self.ep.is_connected() {
            self.ep.disconnect();
            for dg in self.ep.flush() {
                let _ = self.sock.send(&dg);
            }
        }
    }
}

enum Phase {
    LoggingIn { sent: bool },
    Docked { since: Instant },
    Launching,
    Flying(Box<Sortie>),
    Docking,
}

struct Sortie {
    flight: Flight,
    hull: Hull,
    vitals: Vitals,
    /// Attacker of the last 0x03, named as the killer in 0x44.
    last_attacker: u32,
    surroundings: Surroundings,
    /// Own model size for the push test.
    size: f32,
    fields: FieldState,
    ai: Option<Patrol>,
    started: Instant,
    last_full: Option<Instant>,
    last_fast: Instant,
    acc: f32,
    last_step: Instant,
}

pub struct Agent {
    cfg: Config,
    /// Set when the server refuses our login (0x06 rejection code).
    refused: Option<u8>,
    world: Arc<World>,
    models: Arc<Models>,
    /// Other ships in our sector, from the relayed reports.
    sector: Sector,
    /// Stock bots in our sector, some flown by us.
    bots: Bots,
    /// Other pilots' missiles in our sector (for bot dodging).
    missiles: Missiles,
    state: Link,
    space: Option<Link>,
    phase: Phase,
    pid: u32,
    loc: u16,
    ship_type: u8,
    rank: u8,
    /// Launch tube from the last 0x08 (sub 6).
    tube: u8,
    engine: u8,
    /// Fitted shield index (0x06 fittings, 0xfe = none).
    shield: u8,
    clock0: Instant,
    /// Last time another ship's report reached us in our sector.
    company: Option<Instant>,
    cycles_done: usize,
}

impl Agent {
    pub fn connect(cfg: Config, world: Arc<World>, models: Arc<Models>) -> io::Result<Agent> {
        let state = Link::open("state", SocketAddr::new(cfg.server, cfg.state_port))?;
        Ok(Agent {
            cfg,
            refused: None,
            world,
            models,
            sector: Sector::default(),
            missiles: Missiles::default(),
            bots: Bots::new(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64)),
            state,
            space: None,
            phase: Phase::LoggingIn { sent: false },
            pid: 0,
            loc: 0,
            ship_type: 0,
            rank: 0,
            tube: 0,
            engine: 0xfe,
            shield: 0xfe,
            clock0: Instant::now(),
            company: None,
            cycles_done: 0,
        })
    }

    fn log(&self, s: &str) {
        eprintln!("[{}] {s}", self.cfg.callsign);
    }

    fn clock(&self) -> f32 {
        self.clock0.elapsed().as_secs_f32()
    }

    /// Run until the configured cycles are done (Ok) or the session fails.
    pub fn run(&mut self) -> io::Result<()> {
        loop {
            for ev in self.state.poll()? {
                match ev {
                    Event::Connected => {}
                    Event::Message(m) => self.on_state(&m.body),
                    Event::Disconnected => return Err(io::Error::new(io::ErrorKind::ConnectionReset, "server closed state")),
                }
            }
            if let Some(code) = self.refused {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("login refused (code 0x{code:02x})")));
            }
            let space_events = match self.space.as_mut() {
                Some(l) => l.poll(),
                None => Ok(Vec::new()),
            };
            match space_events {
                Ok(evs) => {
                    for ev in evs {
                        match ev {
                            Event::Connected => self.on_space_connected(),
                            Event::Message(m) => self.on_space(&m.body),
                            Event::Disconnected => {
                                self.log("space closed by server");
                                self.space = None;
                            }
                        }
                    }
                }
                Err(e) => {
                    self.log(&format!("space: {e}"));
                    self.space = None;
                }
            }
            if self.tick() {
                self.state.send(&[0xc8]); // logout
                self.state.flush();
                return Ok(());
            }
            self.state.flush();
            if let Some(l) = self.space.as_mut() {
                l.flush();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn on_state(&mut self, m: &[u8]) {
        match m.first() {
            Some(0x06) => {
                if let Some(code) = proto::parse_login_rejection(m) {
                    self.log(&format!("login refused (0x06 code 0x{code:02x}; 0xce = wrong password)"));
                    self.refused = Some(code);
                } else if let Some(c) = proto::parse_char_record(m) {
                    self.pid = c.pid;
                    self.ship_type = c.ship_type;
                    self.rank = c.rank;
                    self.engine = c.fittings[24];
                    self.shield = c.fittings[28];
                    if let Phase::LoggingIn { .. } = self.phase {
                        self.loc = c.loc;
                        self.log(&format!("logged in: player {} at {} in ship {}, {} credits", c.pid, c.loc, c.ship_type, c.credits));
                        self.phase = Phase::Docked { since: Instant::now() };
                    }
                }
            }
            Some(0x08) => {
                if let Some(lc) = proto::parse_location_change(m) {
                    self.on_location_change(lc);
                }
            }
            Some(0x04) => {
                if let Some(t) = proto::chat_text(m) {
                    if self.cfg.verbose || matches!(self.phase, Phase::LoggingIn { .. }) {
                        self.log(&format!("chat: {t}"));
                    }
                }
            }
            Some(0x05) if m.len() == proto::LOC_REPORT_LEN => {
                if proto::u16_at(m, proto::LOC_LOC_OFF) == self.loc && proto::u16_at(m, proto::LOC_SHIP_OFF) != proto::SHIP_GONE {
                    self.company = Some(Instant::now());
                }
                let now = self.clock();
                self.sector.on_message(m, self.loc, self.pid, now);
            }
            Some(0x33 | 0x34) => {
                self.company = Some(Instant::now());
                let now = self.clock();
                self.sector.on_message(m, self.loc, self.pid, now);
            }
            Some(0x03) => self.on_damage(m),
            Some(0x70) => {
                // A spawn order: only a pilot in space takes one.
                let (loc, now) = (self.loc, self.clock());
                let bounce = match self.phase {
                    Phase::Flying(_) => self.bots.on_spawn_order(m, loc, now),
                    _ if m.len() >= 4 => Some(proto::bot_gone(m[1], proto::u16_at(m, 2))),
                    _ => None,
                };
                if let Some(b) = bounce {
                    self.state.send(&b);
                } else if self.cfg.verbose {
                    self.log(&format!("spawn order: bot type {}", 219 + m[1] as u16));
                }
            }
            Some(0x32) => {
                if matches!(self.phase, Phase::Flying(_)) && self.missiles.on_message(m, self.pid) && self.cfg.verbose {
                    self.log("missile lock warning");
                }
            }
            Some(0x71) => {
                if !matches!(self.phase, Phase::Flying(_)) {
                    return;
                }
                let (pid, loc, now) = (self.pid, self.loc, self.clock());
                let before = self.bots.flown();
                if let Some(reply) = self.bots.on_botloc(m, pid, loc, &self.world, now) {
                    if let Some(sp) = self.space.as_mut() {
                        sp.send(&reply);
                    }
                }
                if self.bots.flown() != before {
                    self.log(&format!("now flying {} bot(s)", self.bots.flown()));
                }
            }
            _ => {}
        }
    }

    /// Space-link messages: the same dispatcher as the state link's on the
    /// stock client; the server relays on state, so only damage matters here.
    fn on_space(&mut self, m: &[u8]) {
        if m.first() == Some(&0x03) {
            self.on_damage(m);
        }
    }

    /// S→C 0x03 for our own ship (bot id 0xfe): shields first, overflow to
    /// hull, hull ≤ 0 is death. Ignored unless flying, as the client ignores
    /// it once dead or docked (`0xbe5bbc`, `0xbe5af6`).
    fn on_damage(&mut self, m: &[u8]) {
        let Some(d) = proto::parse_damage(m) else { return };
        if d.bot != proto::NOT_A_BOT {
            for m in self.bots.on_damage(d.bot, d.attacker, d.amount, self.pid) {
                if m[0] == 0x44 {
                    self.log(&format!("bot #{} destroyed by {}", d.bot, d.attacker));
                }
                self.state.send(&m);
            }
            return;
        }
        let Phase::Flying(s) = &mut self.phase else { return };
        s.last_attacker = d.attacker;
        let killed = s.vitals.damage(d.amount);
        let v = s.vitals;
        if self.cfg.verbose || killed {
            self.log(&format!(
                "hit by {} for {:.0}: shields {:.0}/{:.0}, hull {:.0}/{:.0}",
                d.attacker, d.amount, v.shields, v.shields_max, v.hull.max(0.0), v.hull_max
            ));
        }
        if killed {
            self.die();
        }
    }

    /// `player_death` (`0x9cec40`) on the wire: 0x44 naming the killer, then a
    /// dock with target 0xfe. The server sends us home (0x08 to our last base,
    /// no space), and the usual docked wait → relaunch follows.
    fn die(&mut self) {
        let Phase::Flying(s) = &self.phase else { return };
        let killer = s.last_attacker;
        self.log(&format!("destroyed by {killer} in {}", self.loc));
        self.drop_bots();
        self.state.send(&proto::kill_report(proto::NOT_A_BOT, self.pid, killer, -2));
        self.state.send(&proto::travel(ACT_DOCK, self.loc, proto::DEATH_DOCK_ARG, self.pid));
        self.phase = Phase::Docking;
    }

    /// Our ship leaves space: the bots we fly lose their target and go, and
    /// queued spawn orders bounce (as the client's `0x9cb9c0` / `0x9cb930`).
    fn drop_bots(&mut self) {
        self.missiles.clear();
        for m in self.bots.clear() {
            self.state.send(&m);
        }
    }

    fn on_location_change(&mut self, lc: proto::LocationChange) {
        self.drop_bots();
        match lc.space_ip {
            Some(ip) => {
                if ip.parse::<IpAddr>().ok() != Some(self.cfg.server) {
                    self.log(&format!("0x08 names space server {ip}; using {} (the configured server)", self.cfg.server));
                }
                self.loc = lc.loc;
                self.tube = lc.tube;
                self.company = None;
                self.sector.clear();
                match Link::open("space", SocketAddr::new(self.cfg.server, self.cfg.space_port)) {
                    Ok(l) => self.space = Some(l),
                    Err(e) => self.log(&format!("space connect: {e}")),
                }
                self.start_sortie();
            }
            None => {
                self.space = None;
                self.loc = lc.loc;
                self.cycles_done += 1;
                self.log(&format!("docked at {} (cycle {})", lc.loc, self.cycles_done));
                self.phase = Phase::Docked { since: Instant::now() };
            }
        }
    }

    fn start_sortie(&mut self) {
        let hull = self.world.hull(self.ship_type as u16).cloned().unwrap_or_else(|| Hull {
            type_id: self.ship_type as u16,
            name: "?".into(),
            mass: 10_000.0,
            drag: 15.0,
            armour: 5000.0,
            pitch: 0.001,
            yaw: 0.001,
            roll: 0.001,
            engines: 1,
            ..Default::default()
        });
        let thrust = self.world.engine_thrust.get(self.engine as usize).copied().unwrap_or(DEFAULT_ENGINE_THRUST);
        let (flight, ai) = match self.world.station(self.loc) {
            Some((st, sp)) => {
                let route = Route::around(st, sp, self.loc, self.cfg.laps, self.tube, &hull);
                self.log(&format!(
                    "launch tube {}; patrol ring {:.0} u, {} waypoints",
                    self.tube,
                    route.radius,
                    route.ring.len()
                ));
                let mut f = Flight::new(&hull, thrust, route.launch, route.launch_fwd);
                f.tube = Some(route.tube);
                (f, Some(Patrol::new(route)))
            }
            None => {
                self.log(&format!("no station geometry for {}; will just sit and dock", self.loc));
                (Flight::new(&hull, thrust, [0.0; 3], [0.0, 0.0, 1.0]), None)
            }
        };
        let shield = self.world.shield(self.shield);
        let vitals = Vitals::new(&hull, shield);
        let surroundings = Surroundings::new(&self.world, &self.models, self.loc);
        let size = collide::model_size(&self.models, hull.type_id);
        if self.cfg.verbose {
            self.log(&format!("{} collidable objects here", surroundings.obstacles.len()));
        }
        self.log(&format!(
            "launched into {} ({}); shields {:.0}, hull {:.0}",
            self.loc,
            self.world.name(self.loc),
            vitals.shields_max,
            vitals.hull_max
        ));
        let now = Instant::now();
        self.phase = Phase::Flying(Box::new(Sortie {
            flight,
            hull,
            vitals,
            last_attacker: 0,
            surroundings,
            size,
            fields: FieldState::default(),
            ai,
            started: now,
            last_full: None,
            last_fast: now,
            acc: 0.0,
            last_step: now,
        }));
    }

    fn on_space_connected(&mut self) {
        // The server binds this space session by the pid in our first 0x05.
        if let Phase::Flying(s) = &mut self.phase {
            s.last_full = None;
        }
    }

    /// Advance timers and the flight. True when it's time to log out.
    fn tick(&mut self) -> bool {
        let now = Instant::now();
        match &mut self.phase {
            Phase::LoggingIn { sent } => {
                if !*sent && self.state.ep.is_connected() {
                    *sent = true;
                    // Never log the login message: it holds the password.
                    let m = proto::login(&self.cfg.callsign, &self.cfg.password);
                    self.state.send(&m);
                }
            }
            Phase::Docked { since } => {
                if self.cfg.cycles != 0 && self.cycles_done >= self.cfg.cycles {
                    return true;
                }
                if since.elapsed() >= self.cfg.dock_wait {
                    let m = proto::travel(ACT_LAUNCH, self.loc, 0, self.pid);
                    self.state.send(&m);
                    self.phase = Phase::Launching;
                }
            }
            Phase::Launching | Phase::Docking => {}
            Phase::Flying(_) => self.fly(now),
        }
        false
    }

    fn fly(&mut self, now: Instant) {
        let clock = self.clock();
        let (pid, loc, rank) = (self.pid, self.loc, self.rank);
        let callsign = self.cfg.callsign.clone();
        let company = self.company.is_some_and(|t| t.elapsed() < COMPANY_TIMEOUT);
        let (sector, models, world, bots_tbl, missiles) = (&mut self.sector, &self.models, &self.world, &mut self.bots, &mut self.missiles);
        let mut bot_out = bots::Out::default();
        let Phase::Flying(s) = &mut self.phase else { return };

        s.acc += (now - s.last_step).as_secs_f32().min(0.5);
        s.last_step = now;
        let mut dock = false;
        let mut crashed = false;
        let mut bumped = None;
        while s.acc >= DT {
            s.acc -= DT;
            s.vitals.regenerate(DT);
            let order = match s.ai.as_mut() {
                Some(ai) => ai.think(&s.flight),
                None if s.started.elapsed() > Duration::from_secs(10) => Order::Dock,
                None => Order::Fly { toward: s.flight.fwd, throttle: 0.0 },
            };
            match order {
                Order::Fly { toward, throttle } => {
                    s.flight.throttle = throttle;
                    s.flight.turn_toward(toward, DT);
                    // The client's frame: velocity, collisions, then position.
                    s.flight.dynamics(DT);
                    let fr = collide::step(&mut s.flight, s.hull.ext, s.size, &s.surroundings, sector, models, &mut s.fields, DT);
                    if fr.hit.is_some() {
                        bumped = fr.hit.map(|k| (k, fr.impact));
                    }
                    if s.vitals.damage(fr.impact + fr.burn) {
                        crashed = true;
                        break;
                    }
                    s.flight.integrate(DT);
                    sector.advance(DT, clock);
                    let me = bots::Me {
                        pid,
                        pos: s.flight.pos,
                        vel: s.flight.vel,
                        radius: s.hull.ext.iter().fold(0.0f32, |a, &e| a.max(e)) * 0.5,
                    };
                    missiles.step(DT, world, Some((pid, me.pos, me.vel)), sector, |id| bots_tbl.motion(id));
                    let o = bots_tbl.step(DT, clock, Some(me), loc, world, &s.surroundings, sector, missiles);
                    bot_out.state.extend(o.state);
                    bot_out.space.extend(o.space);
                    if o.hit_me > 0.0 {
                        if let Some(a) = o.attacker {
                            s.last_attacker = a;
                        }
                        if s.vitals.damage(o.hit_me) {
                            bot_out.hit_me = -1.0; // killed by a bot
                            break;
                        }
                    }
                }
                Order::Dock => {
                    dock = true;
                    break;
                }
            }
        }
        for m in &bot_out.state {
            self.state.send(m);
        }
        if let Some(sp) = self.space.as_mut() {
            for m in &bot_out.space {
                sp.send(m);
            }
        }
        let Phase::Flying(s) = &mut self.phase else { return };
        if bot_out.hit_me < 0.0 {
            self.die();
            return;
        }
        if let Some((kind, dmg)) = bumped {
            let v = s.vitals;
            eprintln!("[{callsign}] hit {kind:?} for {dmg:.0}: shields {:.0}, hull {:.0}", v.shields, v.hull.max(0.0));
        }
        if crashed {
            // As the client: the impact packet, then player_death.
            self.state.send(&proto::impact_death(loc, pid));
            self.die();
            return;
        }

        let space_up = self.space.as_ref().is_some_and(|l| l.ep.is_connected());
        if space_up {
            let view = s.flight.view(&callsign, loc, &s.hull, rank, &s.vitals);
            let space = self.space.as_mut().unwrap();
            if s.last_full.map_or(true, |t| t.elapsed() >= FULL_REPORT_EVERY) {
                s.last_full = Some(now);
                space.send(&proto::ship_report(&view, pid, clock));
                // As the client: a botloc for each bot we fly after each 0x05.
                for m in self.bots.botlocs(pid, loc) {
                    space.send(&m);
                }
                self.state.send(&proto::companion_report());
            } else if company && s.last_fast.elapsed() >= FAST_REPORT_EVERY {
                s.last_fast = now;
                space.send_unreliable(&proto::ship_small_loc(&view, pid, clock));
            }
        }

        if dock {
            // As the client: zero motion, cargo report (flag 0), then dock.
            let empty = Character {
                callsign: callsign.clone(),
                faction: 0,
                rank: 0,
                ship_type: self.ship_type,
                fittings: Default::default(),
                items: Vec::new(),
                credits: 0,
                experience: 0,
            };
            self.state.send(&proto::cargo_report(&empty, pid, 0));
            self.drop_bots();
            self.state.send(&proto::travel(ACT_DOCK, loc, 0, pid));
            self.log(&format!("docking at {loc}"));
            self.phase = Phase::Docking;
        }
    }
}
