//! jumpgate-server — a standalone preservation server for the stock Jumpgate
//! client.
//!
//! SCOPE & ETHICS (see CLAUDE.md): this is its own server. It never talks to
//! jumpgate-tri.org. Players reach it only by opting in with the Msb32 shim's
//! `SERVER_IP` override. It never sends 0xFE (client memory write), never
//! stores or logs passwords, and rewrites every space-server address it sends
//! to its own.
//!
//! Usage:
//!   jumpgate-server [--templates <jgcap.bin>] [--data <dir>] [--listen ip] [--advertise ip]
//!                   [--state-port 5150] [--space-port 5151] [--home-station 7]
//!                   [--accounts accounts.json] [--npcs] [--verbose]
//!
//! `--data` is the extracts directory (default: the workspace's `notes/extracts`):
//! locations and the gate graph.
//! `--templates` points at a local capture whose server→client messages are
//! used for responses not yet built from scratch (see templates.rs). Without
//! one, those responses are skipped and the client will likely stall at login.
//! `--accounts` is the pilot snapshot (default `accounts.json` in the working
//! directory; keep it out of git). It holds only argon2id password hashes.
//! `--npcs` turns on the old in-server demo NPC; NPCs are now separate
//! `jumpgate-agent` processes that log in like players.

mod accounts;
mod feed;
mod game;
mod net;
mod npc;
mod templates;

use jumpgate_common::{proto, ship, world};

use game::{Config, Conn, Server};
use npc::{NpcSim, Orbit};
use net::{Listener, NetEvent};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use templates::Templates;
use world::World;

fn usage() -> ! {
    eprintln!(
        "usage: jumpgate-server [--templates <jgcap.bin>] [--data <dir>] [--listen ip] [--advertise ip] \
         [--state-port 5150] [--space-port 5151] [--home-station 7] [--accounts accounts.json] [--npcs] [--verbose]"
    );
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut templates_path: Option<String> = None;
    let mut data_dir: Option<String> = None;
    let mut listen: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    let mut advertise: Option<Ipv4Addr> = None;
    let mut state_port = 5150u16;
    let mut space_port = 5151u16;
    let mut cfg = Config::default();
    let mut verbose = false;
    let mut npcs_on = false;
    let mut accounts_path = String::from("accounts.json");
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--templates" => templates_path = Some(val()),
            "--data" => data_dir = Some(val()),
            "--listen" => listen = val().parse().unwrap_or_else(|_| usage()),
            "--advertise" => advertise = Some(val().parse().unwrap_or_else(|_| usage())),
            "--state-port" => state_port = val().parse().unwrap_or_else(|_| usage()),
            "--space-port" => space_port = val().parse().unwrap_or_else(|_| usage()),
            "--home-station" => cfg.home_station = val().parse().unwrap_or_else(|_| usage()),
            "--npcs" => npcs_on = true,
            "--no-npcs" => npcs_on = false,
            "--accounts" => accounts_path = val(),
            "--verbose" | "-v" => verbose = true,
            _ => usage(),
        }
    }
    let advertise = advertise.unwrap_or(match listen {
        IpAddr::V4(v4) if !v4.is_unspecified() => v4,
        _ => Ipv4Addr::LOCALHOST,
    });
    cfg.advertise = advertise.to_string();
    if space_port != 5151 {
        eprintln!("[warn] the client always connects space to port 5151 (0x9c7190); --space-port {space_port} is for testing only");
    }

    let tpl = match &templates_path {
        Some(p) => match std::fs::read(p) {
            Ok(b) => {
                let t = Templates::from_capture_bytes(&b);
                eprintln!("[server] {} template types from {p}", t.len());
                t
            }
            Err(e) => {
                eprintln!("[server] can't read templates {p}: {e}");
                std::process::exit(1);
            }
        },
        None => {
            eprintln!("[warn] no --templates: responses without a builder (0x06 body, listings, 0x8c, …) will be stubs or skipped");
            Templates::default()
        }
    };

    let data_dir = data_dir.unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts").into());
    let world = World::load(std::path::Path::new(&data_dir)).unwrap_or_else(|e| {
        eprintln!("[server] can't load world data from {data_dir}: {e} (use --data <notes/extracts>)");
        std::process::exit(1)
    });
    eprintln!("[server] world: {} locations, {} gate links from {data_dir}", world.names.len(), world.gates.len());

    let mut sim = NpcSim::new();
    if npcs_on {
        spawn_demo_npcs(&mut sim, &world);
    }
    let mut server = Server::new(cfg, tpl, world);
    server.accounts = accounts::Accounts::open(accounts_path.clone().into()).unwrap_or_else(|e| {
        eprintln!("[server] can't load accounts {accounts_path}: {e}");
        std::process::exit(1)
    });
    eprintln!("[server] {} accounts from {accounts_path}", server.accounts.len());
    let mut state = Listener::bind("state", SocketAddr::new(listen, state_port)).unwrap_or_else(|e| {
        eprintln!("bind state: {e}");
        std::process::exit(1)
    });
    let mut space = Listener::bind("space", SocketAddr::new(listen, space_port)).unwrap_or_else(|e| {
        eprintln!("bind space: {e}");
        std::process::exit(1)
    });
    eprintln!("[server] advertising space server {advertise}; point clients here with SERVER_IP=<this host>");

    let start = Instant::now();
    let mut last_tick = start;
    loop {
        let mut outs = Vec::new();
        // NPCs: simulate and publish at 5 Hz, roughly a real client's 0x33 rate
        // in company (message-types "Location update path").
        if last_tick.elapsed() >= NPC_TICK {
            let now = Instant::now();
            sim.tick((now - last_tick).as_secs_f32());
            last_tick = now;
            outs.extend(server.present_ships(sim.ships(), (now - start).as_secs_f32()));
        }
        for (conn, ev) in state
            .poll()
            .into_iter()
            .map(|e| (Conn::State, e))
            .chain(space.poll().into_iter().map(|e| (Conn::Space, e)))
        {
            match ev {
                NetEvent::Connected(a) => {
                    eprintln!("[{conn:?}] {a} connected");
                    if conn == Conn::Space {
                        server.space_connected(a);
                    }
                }
                NetEvent::Message(a, m) => {
                    if verbose && !is_high_rate(&m) {
                        log_msg(conn, a, &m);
                    }
                    outs.extend(server.handle(conn, a, &m));
                }
                NetEvent::Disconnected(a) => outs.extend(server.disconnected(conn, a)),
            }
        }
        for o in outs {
            // Everything goes on the state connection for now: the client has one
            // dispatcher for both (network-protocol.md §1.1a) and the replay
            // milestone ran entirely on state. Keepalive answers use their own conn.
            let (listener, addr) = match o.conn {
                Conn::State => (&mut state, server.state_addr(o.to)),
                Conn::Space => (&mut space, server.space_addr(o.to)),
            };
            if let Some(addr) = addr {
                if verbose && !is_high_rate(&o.msg) {
                    eprintln!(
                        "  → {:?} {} 0x{:02x} ({}B)",
                        o.conn,
                        o.to,
                        o.msg.first().copied().unwrap_or(0),
                        o.msg.len()
                    );
                }
                listener.queue(addr, &o.msg);
            }
        }
        state.flush();
        space.flush();
        for (conn, addr) in server.take_kicks() {
            match conn {
                Conn::State => state.close(addr),
                Conn::Space => space.close(addr),
            }
        }
        server.tick(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
    }
}

const NPC_TICK: Duration = Duration::from_millis(200);

/// For now, one freighter circling Quantar Depot station (sector 118, station at
/// the origin, radius ~1000), close enough to see on launch.
fn spawn_demo_npcs(sim: &mut NpcSim, world: &World) {
    let Some(hull) = world.hull(104).cloned() else {
        eprintln!("[npc] hull 104 not in ship_types.csv; no demo NPC");
        return;
    };
    eprintln!("[npc] demo: {} circling Quantar Depot (118)", hull.name);
    sim.spawn("Hauler", hull, 118, Box::new(Orbit::new([0.0, 400.0, 0.0], 2500.0, 80.0)));
}

/// Type and length, plus a short hex prefix — except the login, which holds a
/// password and is never printed.
/// Messages sent several times a second per ship, left out of `--verbose`
/// (both directions, state and space): 0x33 fast position updates.
const HIGH_RATE: &[u8] = &[0x33];

fn is_high_rate(m: &[u8]) -> bool {
    m.first().is_some_and(|t| HIGH_RATE.contains(t))
}

fn log_msg(conn: Conn, from: SocketAddr, m: &[u8]) {
    let ty = m.first().copied().unwrap_or(0);
    if conn == Conn::State && ty == 0x05 && m.len() <= proto::LOGIN_MAX_LEN {
        eprintln!("  ← {conn:?} {from} 0x05 login ({}B) [not shown]", m.len());
        return;
    }
    let hex: Vec<String> = m.iter().take(24).map(|b| format!("{b:02x}")).collect();
    eprintln!("  ← {conn:?} {from} 0x{ty:02x} ({}B) {}", m.len(), hex.join(" "));
}
