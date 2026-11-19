# jumpgate-server TODO

Parked 2026-11-18 while we explore the client further. `docs/server.md` holds
the design and the request → response table. `notes/message-types.md` holds the
wire layouts. Tags: [bin] = static from the client, [cap] = seen in a capture,
[inferred] = needs confirming.

## Before anyone else connects

- [x] **Accounts** (2026-11-19). argon2id hashes, password never logged,
      reuse warning for new pilots, auto-provisioning (server.md "Accounts
      and economy").
- [ ] Gate the `!` dev commands behind GM rights (an account flag).
- [ ] Check what the stock client shows for a refused login.
- [ ] **Server identity.** Make sure the MOTD (0x81) and the login text say
      clearly that this is the preservation server, not tri.
- [ ] **Live test pass with the real client.** Cover what's still untested:
      `!goto` into the sector you're already in, removing a ship that changes
      sector (0xfa), and whether the client's `from_loc` agrees with ours on
      jumps (we log it when it doesn't).

## Sector content: replace replayed templates with per-sector builders

These templates are replayed in every sector today (`game.rs` `reply_tpl`):

- [ ] **0x73 asteroid ore levels** (sent on 0x8a in space). The captured copy is
      the Quantar Depot bitmap, so asteroids elsewhere probably read as empty
      and can't be mined.
  - Build it from `sector_objects_all.csv`: 2 bits per world-object id
    (`object_id`, not `slot`), set 3 for the sector's asteroids, and add 948
    (the vein) in space sectors.
  - Track depletion server-side from the C→S 0x74 mining reports (f32 mined
    @8) against base(type) × level. Round to the nearest third.
  - Send only on sector entry and in reply to C→S 0x75. Each 0x73 makes the
    client's local amounts jump to the new third.
  - Regrowth rate is a design choice.
- [ ] **0x78 building sites** (on launch and jump). Build per sector: 50
      `[b0][cat][item]` entries for world objects 950+i. Start with all
      zero (nothing built), or use a server-owned list of player buildings.
- [ ] **0x58 mineral vein** (not sent at all yet). Per space sector (loc ≥ 201):
      commodity, asteroid type, obfuscated position (i32 ^ 0xc0b55, relative to
      the sector origin). Artifacts are optional (@0x10/0x11/0x14.., models
      0x190–0x194). Decide where veins sit and how often they move.
      Every 0x58 also replaces the sector's artifact (byte 0x11 < 80 = model
      and skin). Pickup is C→S 0x4F (own position only, after 4 s of InSight
      beam with a Displacer or Burglar fitted): check range, award one of
      cargo items 285–316, and resend 0x58 with 0x20 = 1 to move it.
- [ ] **0xA7 station list** (not sent yet): build per sector.
- [ ] **0x87 particle clouds**: these are universe-wide, so one list for
      everyone is correct. Optionally the server could own the list and drift
      it.
- [ ] **0x61 beacon**: currently only for sectors 201–999. Check what it should
      carry per sector.

## Economy

- [x] Listings, buy/sell, fitting (0x66) and ship change (0x08) on the account,
      flat 99/99 (2026-11-19). Not yet tried with the real client.
- [ ] Real per-station stock and prices (design), stock depletion.
- [ ] Check the starter Storm loadout in the client's fitting screen.
- [ ] 0x4B contracts (trade and mining); award experience (0x92).
- [ ] Ore sale prices must follow the client's mineral-composition formula
      (`compute_ore_prices` 0x449690; message-types "Station market").
- [ ] **Don't trust cargo reports.** C→S 0x0F cargo and 0x74 mining amounts are
      client-controlled. Cap them by mining time × laser rate × asteroid
      remaining before crediting anything.
- [ ] **0xB3 own-ship state is a replayed template: build it.** Today every
      0x17 gets the captured 0xB3, which resets fuel to 100 every ~13 s and
      zeroes ammo in every gun slot. Send fuel 100 and full magazines on the
      first 0x17 after a launch (hull = ship-type armour × 3000 repairs it),
      then echo the client's 0x05 fuel (@0xac) and hull (@0xa4). See message-types
      "Consumables and servicing".
- [ ] C→S 0x0E consumables used (decoded): log it and charge from the server's
      own view if prices are wanted. S→C 0xA9 `[a9][0][r][a][h]` refuel/rearm/
      repair is there for station services.
- [ ] Ammo: 0x05 doesn't report it, so the server must track ammo itself (or
      fill magazines on launch). 0xB3 always overwrites all 8 ammo cells.

## Comms

- [ ] Mayday: C→S `0xC3` sub `0xF6` (faction) / `0xF5` (squad), 2 bytes with
      no position. Broadcast a distress message with the sender's sector and
      position to their faction or squad. The client limits it to one per
      sector visit (physics.md §9 item 7).

## Persistence

- [x] Player state (ship, fittings, items, credits, exp, loc, last base) in
      `accounts.json`, snapshot once a minute and on logout (2026-11-19).
- [ ] World state (asteroid levels, building sites, veins) saved too, if it is
      meant to evolve.

## NPCs (see docs/agents.md)

- [x] Direction chosen 2026-11-19: NPCs are client-impersonating agents
      (`jumpgate-agent`), decoupled from the server. First behaviour: station
      patrol + dock.
- [ ] Live check with the real client: agents visible, smooth, removed on dock.
- [ ] Agent roadmap: weapons and combat, collisions, gate travel, traders,
      miners, pirates (agents.md "Next").
- [ ] Remove the in-server `npc/` + `feed.rs` sim (now behind `--npcs`) once
      agents cover it.
- [ ] Interest management in `feed.rs`: the 190 remote-slot budget (10–199) is
      shared by players, bots and missiles.
- [ ] `!ai` (dev spawn via 0x70) and the 0x71 botloc relay are in but untested
      live. Check with two clients that the second sees the bot move (the
      existing-bot branch of `0x9d6f25` may not take position). Then: credit
      0x44 kills, and hand off (0x2C → S→C 0x71 with a new controller) when the
      owner leaves the sector.
- [ ] Conflux: spawn with S→C 0x70 on sector entry, take C→S 0x44 deaths, then
      handle 0x71 botloc / 0x2C handoff (currently ignored).
- [ ] Combat authority: C→S 0x1D hit queue → S→C 0x03 damage. Check who
      resolves bot fire before relying on it.

## Transport / RakNet

- [ ] Split packets (send and receive). Large messages will need them.
- [ ] Check whether any client handler looks at the connection tag
      (`[pkt+0x10]`); today all replies go on the state connection.

## Housekeeping

- [ ] Keep removing template types as builders land. The goal is for
      `--templates` to become optional and then go away.
- [ ] Grow the request table from the `--verbose` "unhandled … first seen"
      log lines.
- [ ] Keep `docs/server.md` in sync as items close.
