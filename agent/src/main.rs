//! jumpgate-agent — NPC pilots that are clients.
//!
//! Each agent logs in to a jumpgate-server over the normal client protocol,
//! with its own account, sockets and thread, and flies with the client's
//! flight model. The server can't tell it from a player. See docs/agents.md.
//!
//! SCOPE (CLAUDE.md): point agents only at your own server. They never use the
//! space address a server sends; space always goes to `--server`'s host.
//!
//! Usage:
//!   jumpgate-agent [--server 127.0.0.1] [--state-port 5150] [--space-port 5151]
//!                  [--name Agent] [--count 1] [--laps 1] [--dock-wait 20]
//!                  [--cycles 0] [--data <notes/extracts>] [--verbose]
//!
//! The password comes from `JG_AGENT_PASSWORD` (or `--password`, which other
//! users on the machine can see in the process list). Callsigns are
//! `<name>01`, `<name>02`, …; the server provisions each on first login.

mod ai;
mod physics;
mod session;

use jumpgate_common::world::World;
use session::{Agent, Config};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

const RETRY_AFTER: Duration = Duration::from_secs(10);

fn usage() -> ! {
    eprintln!(
        "usage: jumpgate-agent [--server ip] [--state-port 5150] [--space-port 5151] [--name Agent] \
         [--count 1] [--laps 1] [--dock-wait 20] [--cycles 0] [--data dir] [--password pw] [--verbose]\n\
         password: set JG_AGENT_PASSWORD (preferred) or pass --password"
    );
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut server: IpAddr = "127.0.0.1".parse().unwrap();
    let (mut state_port, mut space_port) = (5150u16, 5151u16);
    let mut name = String::from("Agent");
    let mut count = 1usize;
    let mut laps = 1usize;
    let mut dock_wait = 20u64;
    let mut cycles = 0usize;
    let mut data_dir: Option<String> = None;
    let mut password = std::env::var("JG_AGENT_PASSWORD").ok();
    let mut verbose = false;
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--server" => server = val().parse().unwrap_or_else(|_| usage()),
            "--state-port" => state_port = val().parse().unwrap_or_else(|_| usage()),
            "--space-port" => space_port = val().parse().unwrap_or_else(|_| usage()),
            "--name" => name = val(),
            "--count" => count = val().parse().unwrap_or_else(|_| usage()),
            "--laps" => laps = val().parse().unwrap_or_else(|_| usage()),
            "--dock-wait" => dock_wait = val().parse().unwrap_or_else(|_| usage()),
            "--cycles" => cycles = val().parse().unwrap_or_else(|_| usage()),
            "--data" => data_dir = Some(val()),
            "--password" => password = Some(val()),
            "--verbose" | "-v" => verbose = true,
            _ => usage(),
        }
    }
    let Some(password) = password.filter(|p| !p.is_empty()) else { usage() };
    // Callsign field holds 10 characters; leave room for the two-digit suffix.
    if name.is_empty() || name.len() > 8 || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        eprintln!("--name must be 1–8 letters/digits");
        std::process::exit(2);
    }
    let data_dir = data_dir.unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts").into());
    let world = Arc::new(World::load(std::path::Path::new(&data_dir)).unwrap_or_else(|e| {
        eprintln!("can't load world data from {data_dir}: {e}");
        std::process::exit(1)
    }));

    let threads: Vec<_> = (1..=count)
        .map(|i| {
            let cfg = Config {
                server,
                state_port,
                space_port,
                callsign: format!("{name}{i:02}"),
                password: password.clone(),
                laps,
                // Stagger so a fleet doesn't move in lockstep.
                dock_wait: Duration::from_secs(dock_wait + 3 * (i as u64 - 1)),
                cycles,
                verbose,
            };
            let world = world.clone();
            std::thread::spawn(move || loop {
                let result = Agent::connect(cfg.clone(), world.clone()).and_then(|mut a| a.run());
                match result {
                    Ok(()) => {
                        eprintln!("[{}] done", cfg.callsign);
                        return;
                    }
                    Err(e) => {
                        eprintln!("[{}] session ended: {e}; retrying in {}s", cfg.callsign, RETRY_AFTER.as_secs());
                        std::thread::sleep(RETRY_AFTER);
                    }
                }
            })
        })
        .collect();
    for t in threads {
        let _ = t.join();
    }
}
