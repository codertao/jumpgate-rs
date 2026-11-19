# jumpgate-server — the reactive fan server

Crate `server/` (binary `jumpgate-server`), started 2026-11-17. It replaces the
open-loop `jumpgate-proxy replay` with a server that answers each client
request. Scope is as in CLAUDE.md: our own server, players opt in through
`SERVER_IP`, no contact with jumpgate-tri.org.

```sh
cargo run -p jumpgate-server -- --templates jgcap.bin --verbose
# then on the game machine:
SERVER_IP=<server host> Jumpgate.exe
```

Options: `--listen ip`, `--advertise ip` (the address put in 0x08 space
redirects; defaults to `--listen`, else 127.0.0.1), `--state-port 5150`,
`--space-port 5151` (the client always uses 5151 for space, so change it only
for tests), `--home-station 18`, `--verbose` (types and hex prefixes; the login
is never printed).

## Layout

| file | role |
|---|---|
| `net.rs` | One UDP socket per port, one RakNet `Endpoint::server()` per peer address, 60 s idle timeout |
| `game.rs` | Session state and handlers. Pure: `(conn, peer, msg) → Vec<Out>`, so it's unit-tested without sockets |
| `proto.rs` | Layouts the server reads/writes: login, 0x07, 0x05 report, 0x06 patcher, 0x08/0x81/0x04/0x61 builders |
| `templates.rs` | Captured S→C messages used as response bodies until each has a builder |
| `world.rs` | Static world from `notes/extracts/` (`--data`, default the workspace copy): location names, gate graph |

## Templates (temporary)

Most S→C bodies are only partly decoded, so the server answers with the first
captured copy of that type and patches the fields it understands. The capture
is read from a local path at startup and never committed.

Never taken from the capture: **0xFE** (client memory write; never sent at
all), **0x81** MOTD and **0x04** chat (other people's words; we build our own).
Every captured 0x06/0x08 has its space address rewritten.

Goal: replace templates one type at a time with builders driven by
`notes/extracts/*.csv` (locations, items, prices once designed).

## Gate travel (from the extract)

`sector_objects_all.csv` jumpgate rows (both models, `jump_ends.x` and
`x_ai_gate.x`) give `(loc, gate_number) → (dest_loc, dest_gate)`. That's **417
links** from the client's own table (`0x146d7a0`, filled by `0x4a7460`).
- C→S 0x07 jump carries gate number − 1.
- The S→C 0x08 sub byte is the arrival gate: the client spawns the ship at that
  contact slot (handler reads `slot = msg[1]`, position = slot pos + random
  offset).
- Verified: all 3 captured jumps resolve to the captured destination, and the
  built 0x08 equals the captured one byte for byte outside the address field
  (`built_jump_redirect_matches_capture`).
- Quirks in the game data, followed as-is: 3 one-way links (268/3 → 309,
  309/1 → 268 whose return goes to 266, 348/1 → 349). 8 gates have no link
  (loc 0, 1000–1005); jumping through one gets a system chat line instead.
- Not yet checked: that the client's `from_loc` agrees with the server's (it's
  logged if not).

## Request → response table (as implemented)

All responses go on the **state** connection, except keepalive answers, which
use their own connection. The client has one dispatcher for both connections,
and the replay milestone ran entirely on state. Whether any handler checks the
`[pkt+0x10]` connection tag is still unverified.

| C→S | conn | Server does | Source |
|---|---|---|---|
| 0x05 (33 B) login | state | new player id; 0x06 (template, patched callsign/loc/pid, space IP cleared), 0x8c, our 0x81, 0x18 | capture |
| 0x09 cmd | state | the listing for that cmd (template, else zero table 896 B) | capture order |
| 0xc1 | state | 0xc2 | capture |
| 0x65 | state | 0x65 ack | capture |
| 0x04 chat | state | relayed verbatim to every other player [inferred] | — |
| 0x63 / 0xc3 / 0x8a | state | 0x62 / 3×0x14 / weather: docked 0x87, in space 0x73+0x87 (0x73 is the captured Depot asteroid bitmap; build per sector, see message-types "Asteroid ore levels"), plus 0x61 beacon only in sectors 201–999 | capture, pairing [inferred] |
| 0x91 / 0x4b | state | 0x91+2×0x76 / 0x4b | capture |
| 0x17 | state | 0x42 + 0xb3 | capture |
| 0x07 launch | state | loc = station+100; 0x06, 0x78, **0x08 → our IP**, dest sector | capture |
| 0x07 jump | state | gate = arg + 1 → extract link → 0x78, 0x08 (our IP, dest loc, sub = arrival gate). Built, no template | extract; byte-identical to captured jumps |
| 0x07 dock | state | loc = sector−100; 0x45, 0x92, 0xba (pid patched), 0x4b, 0x06, 0x8c, **0x08 "none"** | capture |
| 0xc8 | state | logout | — |
| 0x8f | both | 0x8f on the same connection (the client's dispatcher ignores it, `0x9d3006`). **The answer on the space connection is required**: the client sends one per connection every ~16 s, and when the space ones went unanswered it dropped space ("Disconnected from space server") about 55–65 s after connecting [capture 2026-11-19; the timeout constant isn't located in the binary yet] | capture |
| 0x05 (176 B) report | space | stored and relayed to sector-mates as S→C 0x05 (handler `0x9cf7c8` creates/updates that ship). Newcomers are sent everyone's last report | static |
| 0x33 / 0x34 | space | relayed to sector-mates if the id field is the sender's | static |
| 0x71 | space | botloc relayed verbatim as S→C 0x71 (state conn) to sector-mates, only if ship type @0xc is 219–259 or 0xffff (the "no bots" report has 0), controller @0x20 is the sender and loc @0xa is the sender's sector. Others' clients create/update the bot from it. Untested live: whether the existing-bot branch takes position from it (ai.md "Spawning") | static |
| 0x2c | space | ignored (bot handoff not implemented, so the spawner keeps the bot even after leaving) | — |
| 0x16 / 0x44 | any | logged (spawn rejected / bot destroyed); no reward yet | capture (0x44) |

Server-side authority already enforced: player id and loc in relayed 0x05 are
overwritten with the server's values. A space session can only claim a player
whose state session is on the same host. Fast updates must name the sender.
When a player leaves a sector, the others get their last report with ship type
@0x46 = 0xfa [inferred removal value].

Space-session binding: on handshake, the newest in-space player on the same
host with no space session. A later 0x05 report (pid @0x5c) can correct it.

## Not done yet / next

The working checklist is `server/TODO.md` (2026-11-18). This section keeps the
background.

- Live test with the real client (unit, capture-flow and UDP login tests pass;
  the real client hasn't run against it yet).
- Accounts: any password is accepted and dropped. Add salted slow hashes
  (argon2/bcrypt) before anyone else connects.
- Sector content: 0x73 and 0x78 are captured copies replayed in every
  sector. 0x58 (mineral vein) and 0xA7 (station list) aren't sent at all yet. (0x87 is universe-wide particle
  clouds, so one list for everyone is correct. The server could own the cloud
  list and drift it: see message-types "Particle clouds".)
- NPCs: first server-simulated ship done (see "NPCs" below). Next: roles and a
  population, interest management within the slot budget.
- Reconnects: the client drops its space connection on dock and opens a new one
  from the **same UDP port** on the next launch. The listener therefore treats an
  offline `[0A 00]` from a connected peer as a new session: it drops the old
  endpoint (old AES key, old message numbers) and emits a disconnect first.
  Before this fix (2026-11-17), a second launch hung the client waiting for
  space. ID_DISCONNECTION_NOTIFICATION (0x14) also ends a session.
- RakNet `session` (2026-11-17): reliable packets are resent until acked (3×
  smoothed RTT), duplicates are filtered with a wrap-aware window, and a peer
  that leaves a reliable packet unacked for 15 s is dropped. Received
  ordered packets are held for gaps and stale sequenced ones dropped
  (2026-11-18). Live-confirmed: login, launch and relaunch work, so the
  client's ordered index starts at 0 per connection, as in the reference. Still
  to do: split packets. Send reliability (2026-11-18):
  0x33/0x34 position updates go UNRELIABLE_SEQUENCED, everything else
  RELIABLE_ORDERED, both on channel 0. This matches the client's
  `SendGameMessage` (`0x9c9c10`), which passes channel 0 and reliability 1 or 3
  [bin]. Live-confirmed 2026-11-18: NPC motion stays smooth with 0x33 sent
  sequenced, so the client accepts server-sent sequenced packets.
- Economy (from jgcap2, 2026-11-18; see message-types "Station market"): the
  listings, buy/sell (0x09), credits and item list (0x45), experience (0x92) and
  contracts (0x4B) are now decoded well enough to build. The server could
  answer listings from the extracts with its own stock and prices instead of
  the captured template, apply buys and sells to a per-player 0x45, and offer
  contracts. Prices are a design question: the captured ones are a reference
  only.
  Ore sale prices must follow the client's mineral-composition formula
  (message-types "Station market"), and mined amounts arrive in C→S 0x0F
  cargo reports, which the client controls: cap them by mining time and
  laser rate before crediting.
- Conflux: the live server spawns them with S→C 0x70 (ai_type, spawn#) on
  sector entry; the client flies them and reports deaths with C→S 0x44. That's
  the stock-bot path (approach b in CLAUDE.md), seen working in solo play.
- Unhandled types are logged once each. Use `--verbose` sessions to grow the table.

## NPCs  [2026-11-17, first ship; live-tested with the real client]

Layers, each knowing only the one below it:

- `ship.rs`: plain data shared by both sides: `Hull` (from ship_types.csv via
  `World::hull`) and `ShipView` (loc, hull, callsign, mass, pos, vel, fwd/up,
  effective thrust, armour).
- `npc/`: the simulation. `NpcSim` holds NPCs, each with a `Behaviour` trait
  object that updates its `ShipView` every tick. No protocol, network or player
  knowledge. First behaviour: `Orbit` (constant-speed horizontal circle,
  publishing cruise thrust = drag·v² so clients' dead reckoning holds speed).
- `feed.rs`: per-viewer presentation. Given viewers (player, sector) and ships
  each frame, it sends 0x05 on first sight and every 25 frames, 0x33 otherwise,
  and a 0xfa "gone" 0x05 when a ship leaves the viewer's sector. This is where
  interest management and the 190-slot budget will go.
- `game.rs` `Server::present_ships`: picks viewers (in space, space session
  bound, and has sent a 0x05 from this sector, so the client has arrived) and
  maps sim ids to wire pids `0x4000_0000 + id`.
- `main.rs`: ticks the sim and calls `present_ships` at 5 Hz with a monotonic
  clock (seconds since start). `--no-npcs` turns it off.

Demo: one Chinook (type 104) "Hauler" circling Quantar Depot station (sector
118) at radius 2500, y 400, 80 u/s.

Live test 2026-11-17: the NPC appears as a freighter and its motion is smooth,
so a server-built 0x05 + 0x33 is drawn and dead-reckoned like a remote player,
and pid 0x4000_0000 is accepted. Still unconfirmed: 0xfa removal (needs an NPC
that changes sector), and what the copied unknown 0x05 bytes control.

## Developer 0xFE patches  [tried and removed 2026-11-17]

Two user-approved dev patches were tried for faster test flying: ship-type
drag (`0xff7eac + type*0x1e0 + 0x38`) and the 1225 speed tripwire
(`0xac2620`). Removed: the client checksums the ship-type and equipment
tables in flight and exits on a mismatch (Last Code 117, physics.md §9 item 4).
The expected totals are in non-writable `.text`, so stats can't be patched over
0xFE. Faster testing uses the `!goto` dev command instead. The server again
sends no 0xFE at all.

## Chat `!` commands  [2026-11-17]

The client silently drops unknown `/commands` (slash-commands.md), so server
commands use chat text starting with `!`. They arrive as C→S 0x04 (text @0x13)
and are answered privately with a system chat line, never relayed.

| command | effect |
|---|---|
| `!goto <sector id or name> [gate 1-4]` | In space only. Sends the same 0x78 + 0x08 as a gate jump. Names match without case or punctuation (exact, else unique substring). Without a gate: a station sector (101–118) arrives at the launch point (sub 6), others at their lowest gate. |
| `!ai <0-27 \| jelly \| def [1\|2] [sol\|oct\|quan] \| 220-257>` | In space only. Sends S→C 0x70 (state conn) to the caller only, so that client owns the bot and is its target; it appears ~5 s later where the client chooses (ai.md "Spawning"). A number 0–27 is the conflux C-number (ship 230 + n; C0 = `jelly` = Jellyfish 230, per the user); a raw ship type 220–257 is accepted. `def` = the sector faction's Def 1 (220/222/224), `2` for Def 2, or name the faction. Defenders are refused outside locations 101–200 (spawn_bot drops them there). Turrets 258/259 are excluded (spawn# must name a structure). spawn# is a global counter. C→S 0x16 rejects and 0x44 kills are logged only. The owner's C→S 0x71 botlocs are relayed to the rest of the sector (see the request table). Untested live. |
| `!help` | lists commands |

Dev helpers for now: anyone can use them. Gate them behind GM rights once
accounts exist. Untested live: `!goto` into the sector you're already in.

## Fixed: space disconnect after dock + relaunch (2026-11-19)

Symptom: mid-flight "Disconnected from space server", on the second launch of
a session only. Cause, from the client-side capture: after the relaunch every
space-connection 0x8f went unanswered (state ones were fine). Docking didn't
unbind the old space session, so the new connection was bound only by its
first 0x05. Then the old session hit `PEER_TIMEOUT` (60 s after the dock), and
`disconnected()` cleared the player's *current* `space_addr`, so keepalive
answers had nowhere to go. Fixed in `game.rs`:
- dock unbinds the space session;
- `bind_space` forgets the player's previous space address;
- `disconnected(Space, addr)` ignores any session that isn't the player's
  current one.

Test: `stale_space_session_does_not_unbind_the_new_one`. The RakNet layer
wasn't at fault.

