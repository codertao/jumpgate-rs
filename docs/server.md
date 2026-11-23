# jumpgate-server — the reactive fan server

> Bare references to classic-game notes (message-types, physics.md, ai.md,
> slash-commands.md, `extracts/`, …) mean the files in `../notes/`.

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
for tests), `--home-station 7` (where new pilots are provisioned),
`--accounts accounts.json` (pilot snapshot, gitignored), `--npcs` (the old
in-server demo NPC, now off by default), `--verbose` (types and hex prefixes;
the login is never printed).

Shared code lives in the `common` crate (`jumpgate-common`): wire layouts
(`proto.rs`), the pilot model (`character.rs`), `ship.rs` and the extract
loader (`world.rs`). The server and `jumpgate-agent` (agents.md) both use it.

## Layout

| file | role |
|---|---|
| `net.rs` | One UDP socket per port, one RakNet `Endpoint::server()` per peer address, 60 s idle timeout |
| `game.rs` | Session state and handlers. Pure: `(conn, peer, msg) → Vec<Out>`, so it's unit-tested without sockets |
| `accounts.rs` | Pilot accounts: argon2id hash + `Character` + loc/last base, JSON snapshot |
| `templates.rs` | Captured S→C messages used as response bodies until each has a builder |
| `common/proto.rs` | Layouts both sides read/write: login, 0x07, 0x05 report, 0x06 builder, 0x08/0x45/0x92/listings/0x81/0x04/0x61 |
| `common/world.rs` | Static world from `notes/extracts/` (`--data`, default the workspace copy): names, gates, hulls, stations, station points, engines |

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
| 0x05 (33 B) login | state | verify or provision the account (see "Accounts"); new player id; **built 0x06** from the account, docked at its last base; 0x8c, our 0x81, 0x18; a password-reuse warning (0x04) for new pilots. Wrong password: rejection 0x06 code 0xce ("Password or username incorrect"), no session; no callsign: 0xc9 | capture + built |
| 0x09 listing cmd | state | built listing: every entry stock 99, price 99 (equipment 201, the display floor), flag 1 (no surcharge) | built |
| 0x09 buy (04/05/07/0a–10) | state | docked, index in range, 0 < qty ≤ stock, credits ≥ ⌈99·qty⌉, free slot → add to item list, charge; always answers 0x45 | built |
| 0x09 sell (08) | state | index = item-list slot; +⌊99·qty⌋; 0x45. A slot sold out is removed and the rest move up, as on the classic server (`Character::take_item`; fits too); lists saved with gaps are closed up when next used | built |
| 0x66 fitting | state | op 0 unfit (ship slot → list slot), op 1 fit, op 2 missile fired (rack emptied, no reply); 0x45 | built |
| 0x08 change ship | state | docked, known hull, 99 credits → set ship type, anything still fitted goes to the item list; 0x45 + 0x06 | built |
| 0xc1 | state | 0xc2 | capture |
| 0x65 | state | 0x65 ack | capture |
| 0x04 chat | state | relayed verbatim to every other player [inferred] | — |
| 0x63 / 0xc3 / 0x8a | state | 0x62 / 3×0x14 / weather: docked 0x87, in space 0x73+0x87 (0x73 is the captured Depot asteroid bitmap; build per sector, see message-types "Asteroid ore levels"), plus 0x61 beacon only in sectors 201–999 | capture, pairing [inferred] |
| 0x91 / 0x4b | state | 0x91+2×0x76 / 0x4b | capture |
| 0x17 | state | 0x42 + **built 0xB3**: the first after a launch repairs (hull = armour × 3000), refuels (100) and fills magazines (guns.csv `ammo`); later ones echo the client's last 0x05 hull @0xa4 and fuel @0xac so damage sticks. Ammo isn't reported by the client, so magazines are always sent full (TODO.md) | capture (0x42) + built |
| 0x07 launch | state | loc = station+100 (saved to the account); 0x06, 0x78, **0x08 → our IP**, dest sector | capture |
| 0x07 jump | state | gate = arg + 1 → extract link → 0x78, 0x08 (our IP, dest loc, sub = arrival gate). Built, no template | extract; byte-identical to captured jumps |
| 0x07 dock | state | loc = sector−100, saved as the account's last base. **to_loc 0xfe = the pilot died**: the station is the account's last base instead, wherever they died (no ship loss or insurance yet); built 0x45 and 0x92, 0xba (pid patched), 0x4b, 0x06, 0x8c, **0x08 "none"** | capture + built |
| 0xc8 | state | logout; snapshot saved | — |
| 0x0F cargo report | state | ignored (never trust it for credit; TODO.md) | — |
| 0x8f | both | 0x8f on the same connection (the client's dispatcher ignores it, `0x9d3006`). **The answer on the space connection is required**: the client sends one per connection every ~16 s, and when the space ones went unanswered it dropped space ("Disconnected from space server") about 55–65 s after connecting [capture 2026-11-19; the timeout constant isn't located in the binary yet] | capture |
| 0x05 (176 B) report | space | stored and relayed to sector-mates as S→C 0x05 (handler `0x9cf7c8` creates/updates that ship). Newcomers are sent everyone's last report | static |
| 0x33 / 0x34 | space | relayed to sector-mates if the id field is the sender's | static |
| 0x71 | space | botloc relayed verbatim as S→C 0x71 (state conn) to sector-mates, only if ship type @0xc is 219–259 or 0xffff (the "no bots" report has 0), controller @0x20 is the sender, loc @0xa is the sender's sector, and the sender is the bot's controller in the registry (a stale botloc from the previous controller is dropped; an unknown bot is adopted). See "Stock bots" | static + loopback |
| 0x2c | space | bxr hand-off: if the sender is in space in that sector and someone else flies the bot, the bot becomes the sender's and the last botloc, re-addressed to it (@0x20 = @0x24 = sender), goes to the whole sector including the sender. See "Stock bots" | static + loopback |
| 0x32 missile | space | relayed verbatim as S→C 0x32 (state conn) to sector-mates if the owner @0x18 is the sender. Their clients fly it for display and warn its target; the owner's client resolves hits (0x1D). C→S 0x1A (mkill) isn't handled yet | static + capture |
| 0x1D hit | space | → **S→C 0x03** `[03][0][u16 bot][u32 attacker][f32 dmg]` on the victim's state link. Damage: direct gun/missile hits = count (≤ 40) × guns.csv / missiles.csv `damage`; splash and beam hits = the shooter's value. Victim = the 0x1D pid for a player; for a bot (id ≠ 0xfe), the controller from its last botloc. Dropped: object hits (class 1), specials (class 3), unknown weapons, self-hits, victims not in the shooter's sector or docked. No validation of range or rate yet (TODO.md) | built (message-types "Combat") |
| 0x16 / 0x44 | any | the bot is forgotten if the sender is its controller (0x16: spawn bounce or the controller removed it; 0x44: killed). 0x44 with bot id 0xfe (the sender died) is logged. No reward yet | capture (0x44) + static |

Server-side authority already enforced: player id and loc in relayed 0x05 are
overwritten with the server's values. A space session can only claim a player
whose state session comes from the same IP. Fast updates must name the sender.
When a player leaves a sector, the others get their last report with ship type
@0x46 = 0xfa [inferred removal value].

Space-session binding [2026-11-21]: the space link carries no session token
(notes/network-protocol.md §1.1a), so a session is bound by the player id @0x5c
in its first 0x05 report, which the client sends as soon as it connects. The
claim is accepted only if that player is logged in, is in space (not docked),
and its state session comes from the same IP. Ids are sequential, so this is a
minimal check, not authentication. Until then the session is anonymous and its
traffic (including 0x8f) is ignored. Once bound, a session keeps its player: a
report claiming someone else is refused and logged once, not rebound. A player's
new session (relaunch, new port) replaces its old one. The connection's IP alone
identifies nobody (NAT; agents and a player on one host).

Replaced 2026-11-21: binding on handshake to "the newest in-space player on the
same host with no space session". With 30 local agents it bound sessions to the
wrong pilots, leaving the real owner's link unbound ("Space 0x33 from unbound
peer"). Tests: `space_sessions_bind_by_claimed_player_not_by_ip` and the
`space_claim_*` / `bound_space_session_cannot_switch_player` tests.

## Not done yet / next

The working checklist is `server/TODO.md` (2026-11-18). This section keeps the
background.

- Live test with the real client (unit, capture-flow and UDP login tests pass;
  the real client hasn't run against it yet).
- Sector content: 0x73 and 0x78 are captured copies replayed in every
  sector. 0x58 (mineral vein) and 0xA7 (station list) aren't sent at all yet. (0x87 is universe-wide particle
  clouds, so one list for everyone is correct. The server could own the cloud
  list and drift it: see message-types "Particle clouds".)
- NPCs: now separate client processes (agents.md). The in-server demo NPC
  (below) is kept behind `--npcs` for comparison.
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
- Economy: listings, buy/sell, fitting and ship changes are built (flat
  99/99, equipment 201 since 2026-11-20; "Accounts and economy" below). Still to design: real
  stock and prices per station, and contracts (0x4B). The captured prices are
  a reference only.
  Ore sale prices must follow the client's mineral-composition formula
  (message-types "Station market"), and mined amounts arrive in C→S 0x0F
  cargo reports, which the client controls: cap them by mining time and
  laser rate before crediting.
- Conflux: the live server spawns them with S→C 0x70 (ai_type, spawn#) on
  sector entry; the client flies them and reports deaths with C→S 0x44. That's
  the stock-bot path (approach b in CLAUDE.md), seen working in solo play.
- Unhandled types are logged once each. Use `--verbose` sessions to grow the table.

## Stock bots  [2026-11-20; loopback-tested with agents, not yet with the real client]

As in the classic design, the server only books bots; clients fly them
(notes/ai.md §2). `server/src/bots.rs` holds, per (loc, bot id): the ship
type, the controller, the last botloc and position.
- **Spawning:** `!ai` or a population rule sends S→C 0x70 to one pilot in
  the sector. That client flies the bot and is its target. An order with no
  botloc within 30 s is forgotten.
- **Population rules:** `--bots <file>`, one rule per line,
  `loc ship_type count` (`#` comments). Every 20 s at most, a rule below its
  count orders one more bot from a pilot in space in that sector. A sector
  holds at most 40 bots (the client's table holds 50). The spawner is the first
  such pilot; agents aren't preferred yet (the server can't tell them apart).
- **Hand-off:** C→S 0x2C from a pilot in the sector → the bot is theirs; the
  last botloc goes out with @0x20 = @0x24 = the new pilot, to everyone in the
  sector. Every client's 0x71 handler then agrees (target == me → I fly it).
- **Re-homing:** each tick, a bot whose controller isn't in space in its
  sector goes to the nearest pilot still there (by their last 0x05), or is
  forgotten. Usually the old controller's client already removed it (0x16).
- **Damage:** C→S 0x1D hits on a bot go as S→C 0x03 to its controller.
- Not done: rewards for 0x44, choosing agents as spawners, sector-entry
  spawning.

## Accounts and economy  [2026-11-19; unit-tested, not yet tried with the real client]

`accounts.rs`. One account per callsign, case-insensitive.
- **Password:** only an argon2id PHC string (random salt, default parameters)
  is stored. The login message's password is hashed (new account) or verified,
  then dropped. It is never logged, and `--verbose` doesn't print the login.
  A wrong password gets the client's own login rejection: a 0x06 with code
  0xce @0x42 (`proto::login_rejection`), addressed by a one-shot id
  (`REJECTED_BASE`), and no session. The client shows "Password or username
  incorrect" on its Connection Error panel (notes/message-types "0x06 login
  rejection"). A login with no usable callsign gets 0xc9. The state session is
  left for the client to close. Until 2026-11-21 this was a 0x04 chat line,
  which the login screen never shows, so a refused login looked like a hang
  ending in a connection error. Not yet seen on the real client.
- **Provisioning** on the first login of an unknown callsign: Quantar
  (faction 3), docked at Quantar-Core Station (7; `--home-station`), a Storm
  (type 90), 200,000 credits, 0 exp, and a starter loadout of engine 0,
  powerplant 0, radar 1, shield 1 and capacitor 0. That loadout is picked to fit
  a Storm's size-1 mounts and the power budget; it's unchecked in the client's
  fitting screen. New pilots also get a chat line saying not to reuse a
  password, since the game sends it in cleartext.
- **State kept:** callsign, faction, rank, ship type, fittings (30 bytes as in
  0x06 @0x70), 48 item slots, credits, exp, current loc, last base. Logins
  start docked at the last base. Launch, jump and dock update `loc`, and dock
  sets `last_base`.
- **Snapshot:** `--accounts` (default `accounts.json`, gitignored). Written at
  most once a minute when something changed, and on every logout, via a temp
  file and a rename.
- **One session per account:** a second login with the same callsign drops
  the first session.
- **Messages built from the account:** 0x06 (`proto::char_record`; bytes we
  don't model come from the captured template when one is loaded, else from
  captured constants), 0x45, 0x92, and the listings. See message-types "0x06
  character record".
- **Economy rules:** every listing shows stock 99; commodities cost 99 and
  equipment 201 (`Config::price` / `stock`, raised by `proto::listed_price`).
  The client hides any equipment priced ≤ 200 (message-types "Station
  market"), so a flat 99 showed empty equipment tabs (fixed 2026-11-20). Buying more than 99 or more than you can afford is refused, with
  a reason in chat; every buy and sell answers with the current 0x45. Stock
  isn't depleted. Ship changes cost 99. Item indices are the listing entry
  numbers, as in the 0x45 item list.

## NPCs  [2026-11-17, first ship; live-tested with the real client]

> Superseded 2026-11-19 by client-side agents (agents.md). This in-server sim
> now runs only with `--npcs`.

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
  clock (seconds since start). Off unless `--npcs`.

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


## Sessions  [2026-11-20]

- **One session per account.** A login for a callsign that's already
  logged in replaces the old session. The old player is logged out, the
  sector gets its ship-removal report (0x05 with ship type 0xfa) with the new
  login's replies, and its state and space connections are **closed**: the
  game queues them (`Server::take_kicks`) and the network loop sends RakNet's
  ID_DISCONNECTION_NOTIFICATION (`Endpoint::disconnect`, `Listener::close`).
  Until this fix the removal messages were built and thrown away, so
  restarting an agent mid-flight left its old ship hanging in other players'
  sectors.
- A transport timeout or disconnect logs the player out the same way (the
  removal goes out; nothing to close).
- Two processes running the same callsign will keep kicking each other: each
  reconnects (agents retry after 10 s) and replaces the other.

## Launch tubes  [2026-11-20]

S→C 0x08 sub 6 carries the launch tube @2 (notes/launch.md). The server
picks one of the station model's tubes at random (`pick_tube`; all four at
the core stations and depots). This is done for launches and for `relocate`
to a launch slot, which used to send 0xcc there, an out-of-range tube.
