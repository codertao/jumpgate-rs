# Server design notes derived from the classic-game notes

> Bare references to classic-game notes (message-types, physics.md, ai.md,
> slash-commands.md, `extracts/`, …) mean the files in `../notes/`.

What our server should do with each classic mechanism. The mechanisms
themselves are documented in ../notes/message-types.md; this file holds only
our plans and current shortcuts. Moved out of ../notes/ on 2026-11-19.

## 0x78 building sites

**For the server:** build 0x78 per destination sector from a per-sector site
table (default all zero), not the captured template. The current template (the
first capture's Quantar Depot SDS entry) is harmless elsewhere, because type 11
needs a `station7a` station, but it is wrong for 116/117. A conflux eruption can
be shown by putting type 15 on a site.

## 0x73/0x74 asteroid ore levels (capping claims)

**Server implications:** cap 0x74 claims with the same `remaining/base`
diminishing-returns model. Round a rock's level to the nearest third (not up)
so a refresh doesn't visibly refill rocks people just mined. Send 0x73 only
on sector entry and in answer to 0x75.

## 0x73 asteroid ore levels (building it)

**For the server:** build 0x73 per sector. Set level 3 for every object id with
a `mining_divisor` (plus 948 where the sector has it), keep a per-asteroid
remaining amount fed by 0x74 (capped, like 0x0F), send its level (nearest third; see "How depletion looks"), and regrow over time. **Today the server
sends the captured Quantar Depot bitmap everywhere**, so in other sectors
asteroids whose ids aren't asteroids at the Depot get level 0 = 0 ore. Mining
there is probably broken until this is built.

## 0x58 mineral vein and artifact

**For the server:** choose a vein element, type and spot per space sector, and
send 0x58 on sector entry (encode positions as `(int)(pos − origin) ^ 0xc0b55`).
Every 0x58 also places an artifact, so pick byte 0x11 < 80 and a spot for it.
Handle C→S 0x4F: check range to the artifact, add a cargo item, and send a
new 0x58 with 0x20 = 1 to move it.
Today a captured 0x58 goes everywhere, so every space sector gets the same
vein at another sector's coordinates.

## 0xB3 own-ship state

Our server: it replays the first captured 0xB3 as the reply to every 0x17.
That means fuel is reset to 100 about every 13 s (infinite fuel), and **every
ammo-gun slot is set to 0** (the template's ammo is all zero). Build 0xB3 from
server state instead:
- after a launch, fuel 100 and full magazines;
- afterwards, echo the client's 0x05 fuel/hull, or the server's own figure;
- charge for the refill on launch, if prices are wanted.

## 0x06 character record

Our builder starts from the captured 0x06 when `--templates` is loaded (so
unknown bytes match a real record) and otherwise from the captured constants,
then writes every field above that it models. Item categories are as in
"Station market": 0 guns, 1 missiles, 3 commodities, 5–10 engines through
capacitors, 11 devices.

