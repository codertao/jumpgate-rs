# NPC agents (`jumpgate-agent`)

> Bare references to classic-game notes (message-types, physics.md, ai.md,
> slash-commands.md, `extracts/`, …) mean the files in `../notes/`.

Since 2026-11-19, NPCs are **independent client processes**, not something
simulated inside the server. Each agent logs in over the normal client protocol
with its own account, sockets and thread, and runs the simulation a client
would run: flight model, AI and, later, weapons and collisions. The server
treats agents exactly like players and needs no NPC code. Agents can run on
another machine, be restarted on their own, or be swapped for different
behaviours.

```sh
cargo run -p jumpgate-server -- --templates jgcap.bin
JG_AGENT_PASSWORD=<pw> cargo run -p jumpgate-agent -- --server 127.0.0.1 --count 2
```

Options: `--server ip`, `--state-port 5150`, `--space-port 5151`, `--name
Agent` (callsigns `Agent01`, `Agent02`, …, at most 8 characters before the
suffix), `--count N`, `--laps 1`, `--dock-wait 20` (seconds docked before
each launch; agent i waits 3·(i−1) s longer), `--cycles 0` (launch/dock cycles
before logging out; 0 = forever), `--data <extracts>`, `--verbose`. The
password comes from `JG_AGENT_PASSWORD` (`--password` also works, but shows in
the process list). The server provisions each callsign on its first login
(server.md "Accounts and economy").

**Scope:** agents connect only to `--server`. The space connection always goes
to that host, whatever address the server's 0x08 names, so a misconfigured or
foreign server can't redirect them. Point them only at your own server
(CLAUDE.md).

## Crate layout

| file | role |
|---|---|
| `session.rs` | One `Link` (UDP socket + `raknet::session::Endpoint::client()`) per connection. Runs the client message flow, sends keepalives and reports |
| `physics.rs` | `Flight`: the own-ship model from physics.md §3–§4. Thrust/quadratic drag/brakes, kinematic turning at 1000 × rate × turnScale rad/s, fixed 30 Hz step |
| `ai.rs` | `Route` (station geometry → patrol ring) and `Patrol` (state machine) |
| `main.rs` | CLI; one thread per agent, which reconnects 10 s after a failed session |

It shares `jumpgate-common` with the server: `proto` (login, 0x07, 0x05/0x33
reports, 0x0F, 0x17 builders; 0x06/0x08 parsers) and `world` (hulls, engines,
stations, station points).

## Message flow (as the stock client)

1. State connect (5150), then **0x05 login** (33 B; never logged).
2. **0x06**: pid, ship type, loc, fitted engine. The agent is docked.
3. After `dock-wait`: **0x07 launch** `(1, loc, 0, pid)`.
4. **0x08** with sub 6 and a space address: open the space connection (5151)
   and start the sortie at the station's launch point.
5. While flying:
   - a **0x05 report** on space every 5 s (the first one binds the space
     session to our pid on the server), each followed by a **0x17** on state;
   - **0x33** (unreliable sequenced) every 0.3 s, but only while another
     ship's 0x05/0x33/0x34 has reached us in the last 30 s. The real client
     likewise sends fast updates only when a ship is within 10,000 units.
   - **0x8f** keepalive every 10 s on each connection.
6. When the AI says dock: **0x0F** cargo report (flag 0, empty list) and then
   **0x07 dock** `(2, sector, 0, pid)`. This is the order the client uses
   (physics.md §6).
7. **0x08 "none"**: drop space, docked again, back to step 3.

Reports carry the simulated state: position, velocity, fwd/up, effective
thrust (thrust × throttle), mass and full armour. Remote clients dead-reckon
with the same thrust/drag law (physics.md §8), so the agent's motion between
updates should match what they predict.

## Patrol AI (first behaviour)

Shaped like the stock defender (ai.md §2: launch → patrol → return → dock),
with the docking copied from the client's Auto-Dock (autodock.md). Geometry
comes from `extracts/station_points.csv` and the station's bounding radius
(`model_radius`):
- **Launch:** as the client does (notes/launch.md): the ship appears at rest
  in the launch tube the server's 0x08 names (@2), `f142` units down it,
  nose along the tube. Our physics applies the client's tube push (+3 u/s
  per 1/30 s step while inside the tube), and the AI holds the nose straight
  until the ship is out, which takes ~1.3 s and leaves it at ~240 u/s with
  full throttle. It then carries on straight at cruise until 300 units clear
  of the station's bounding sphere. Unit tests fly from every tube at both
  stations.
- **Patrol:** a ring of 12 waypoints per lap, counter-clockwise about the
  station's up axis, in the plane of the dock point. Radius = the largest of
  3000, 2.5 × launch distance, 1.5 × station radius, and beacon distance +
  2500 (5000 at Quantar Core). The ring ends on the docking axis after `laps`
  full turns. Waypoints are flown at top speed (√(thrust/drag), 335 u/s for a
  starter Storm), easing to ≤ 150 u/s when a waypoint is well off the nose
  (a Storm at full throttle turns on an ~850 u radius and would orbit it).
  The last ring point is approached slowly for the turn onto the axis.
- **Run-in (ToBeacon):** inward along the axis to the docking beacon (1500 out
  from `dock_end`, 2550 at Quantar Core), at ≤ 150 u/s and slowing with
  distance. A velocity controller holds the axis line: wanted velocity = along
  the axis plus a sideways term proportional to the error, with the nose
  pointed at that plus a correction for the velocity error (damps the swing).
- **Stop:** within 350 of the beacon (the Auto-Dock's engage range), close
  the throttle and brake to < 2 u/s. The client damps velocity ×0.98 per
  frame instead; we use the real brakes and drag so other clients' dead
  reckoning agrees.
- **Creep:** face the beacon and creep onto it at up to 25 u/s with 22 %
  throttle (the client's state 2), easing off near it; done within 10 units.
- **Final:** down the axis towards `dock_start` at ≤ 70 u/s (state 3), thrust
  only once lined up, same axis-holding controller.
- **Dock:** within 50 units of `dock_end` and under 100 u/s (the client's dock
  test allows < 60 and < 100).

Unit tests fly the whole patrol at the real Quantar Core and Depot geometry
and assert that the ship never enters the station's bounding sphere more than
30 units off the axis (launch excepted) and stays within 30 units of the axis
on the final run. Typical run: stop ~5 off axis, creep to the beacon, final
run ≤ 1 unit off axis, docked after ~230 s.

Reports carry the pilot's rank (0x05 @0x14, from the 0x06) and set the cargo
pod byte (@0x17) only for the Cargo Tow hulls. Before 2026-11-20 both were
copied from a captured Thunder, so every agent showed rank 30 and a pod.

## Status

- 2026-11-19: end to end against our server on loopback. Two agents were
  provisioned, launched into 107, had their space sessions bound, flew the
  patrol, docked at 7 and logged out. The snapshot had Storm / 200,000 /
  last base 7 with argon2id hashes only.
- 2026-11-20, first look with the real client: agents visible and moving.
  Reported and fixed: a cargo pod on every agent, rank 30 shown for rank-0
  agents, and an approach that overshot the axis and would have hit the
  station. The new approach is unit-tested only so far.
- 2026-11-20, second look: docking good. Launch started at rest with no
  catapult and always from one point: fixed (tube push, server-chosen
  tube). Restarting the agents left a second Agent01 in the sector: the
  server dropped the replaced session's ship-removal messages; fixed (see
  server.md "Sessions").
- Approximations: mass = hull + 5000 (fitted equipment isn't summed yet). No
  collision model: the route avoids the station's bounding sphere except down
  the tube.

## Next

- Weapons and combat: targeting, fire (0x1D hit queue / 0x03 damage on the
  server, combat authority still open), missiles (ai.md guidance).
- Collisions with stations and asteroids, using the extracted volumes.
- More behaviours: traders on gate routes (buy, jump, sell, so they also
  drive the economy), miners, pirates, conflux-like hunters.
- Gate travel: 0x07 jump, then arrive at the 0x08's gate slot (needs gate
  positions from the extract).
- Track fittings and cargo from 0x06/0x45 for mass, thrust and trading.
