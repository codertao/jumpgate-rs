# Jumpgate.exe — flight physics / simulation

Static analysis of the client-side ship physics in this build (SHA-256
`fef6e50f…`), done 2026-11-11 with objdump plus Ghidra 12.2 headless
decompilation (`ghidra/scripts/Decomp.java`). Nothing here has been checked at
runtime yet. Unless a line says **[inferred]**, it was read directly from code.

**Headline:** the player's own ship is simulated **entirely on the client**. That
covers thrust, quadratic drag, rotation, collisions, docking, gate pull and
hazard damage. The server gets the results (position, orientation, velocity,
shields/hull) through the location packets (`0x05`/`0x33`/`0x34`). The model is
arcade-Newtonian: velocity persists, thrust acts along the nose, drag is
quadratic in speed, and turning is kinematic (no angular momentum).
Integration is explicit Euler with a variable frame `dt`. Several impulses are
applied per frame, not per second, so they depend on frame rate.

---

## 1. Where the state lives: protected "cells"

Each scalar of the own-ship state is a separately `new`'d **4-byte heap cell**.
A global pointer table points at the cells: `0x14408b0..0x1440954`, plus a few
more at `0x14e86b8..c8`, `0xbe5acc/ad0`, `0xbe6e74` and `0x140ac2c..0x140acf8`.

- `0x403150` creates them. It runs once from WinMain (`0xa24348`).
- **`0x403740` runs at the top of every frame** (main loop `0xa25720`,
  checkpoint `0x3ea`). It reads all ~70 values, frees every cell, and
  re-allocates them in an order that starts at `rand() % 51` (jump table
  `0x404aa8` into an unrolled `new` sequence). Then it writes the values back.
  **The heap addresses change every frame.** This is an anti-memory-editor
  measure: you can't pin a Cheat-Engine-style address to speed or fuel.
- The flight code reaches the cells through a big game object at **`0xbdf578`**
  (passed as `this`/`param_1`). A cell is `**(this + off)`, where `this + off`
  is the pointer global. For example, `0xbdf578 + 0x861380 = 0x14408f8`. This
  is why a plain `ds:0x14408f8` cross-reference search misses most uses.

| pointer global | `this+` | meaning | set by |
|---|---|---|---|
| `0x14408b0` | `0x861338` | **fuel** (fuel cells: drained by afterburner and brakes; HUD shows ×0.01) | launch packet `+0x70`; 0x9cf6b0 cases |
| `0x14408b4` | `0x86133c` | **throttle fraction** = throttle × 0.01 (× 1.3 on afterburner) | `0x429bb0` |
| `0x14408d8` | `0x861360` | **roll rate** (ship-type `+0x44`) | `0x428290`, `0x9d23f9` |
| `0x14408dc` | `0x861364` | **pitch rate** (ship-type `+0x40`) | same |
| `0x14408e0` | `0x861368` | **yaw rate** (ship-type `+0x48`) | same |
| `0x14408f8/fc/900` | `0x861380..88` | **velocity** X/Y/Z (units/s) | `0x429bb0`, collisions |
| `0x1440904` | `0x86138c` | **speed** \|v\| | `0x429bb0` |
| `0x1440908` | `0x861390` | **mass** (hull + equipment + cargo, MODx-adjusted) | `0x428290` |
| `0x144090c` | `0x861394` | **drag coefficient** (ship-type `+0x38`) | `0x428290`, `0x9d2456` |
| `0x1440910` | `0x861398` | **thrust** (engine count × engine thrust, MODx-adjusted) | `0x428290`, `0x4680c0` |
| `0x1440914` | `0x86139c` | **dt**: frame time in seconds = ΔQPC / freq (`0xa25d1d`), unclamped | main loop |
| `0x1440918` | `0x8613a0` | drag deceleration (scratch) = speed² / mass × drag | `0x429bb0` |
| `0x144091c/20/24` | `0x8613a4..ac` | unit velocity direction (scratch) | `0x429bb0` |
| `0x1440928/2c/30` | `0x8613b0..b8` | orientation: **up** axis | `0x42a110` |
| `0x1440934/38/3c` | `0x8613bc..c4` | orientation: **forward / nose** axis (thrust direction) | `0x42a110` |
| `0x1440940/44/48` | `0x8613c8..d0` | orientation: **right** axis | `0x42a110` |
| `0x144094c/50/54` | `0x8613d4..dc` | **position** X/Y/Z (sector coordinates) | `0x9c7270` |
| `0x14e86c0` | `0x909148` | shields | — |
| `0x14e86c4` | `0x90914c` | hull | — |
| `0x14e86bc` | `0x909144` | hull scale for hazard damage (= table × ship-type armour, set at launch) [inferred: max hull] | `0x9cc750` |
| `0xbe5ad0` | `0x6558` | byte: **fuel-additive boost active** (FlashFire: "+200 % thrust for 10 s") | — |

The axis naming follows from the rotation code: yaw input moves the nose toward
`0x1440940` using the yaw rate, and pitch input moves it toward `0x1440928`
using the pitch rate. The ship's collision box pairs forward with ship-type
`+0x160`, up with `+0x15c` and right with `+0x158` (half-extents).

Plain (non-cell) globals used by the flight code:

| addr | `this+` | meaning |
|---|---|---|
| `0x1440878` | `0x861300` | **yaw stick**, int −100..100 |
| `0x144087c` | `0x861304` | **pitch stick**, int −100..100 |
| `0x1440880` | `0x861308` | **roll stick**, int −100..100 |
| `0x1440884` | `0x86130c` | **throttle**, int 0..100 (values below 6 are forced to 0) |
| `0x144088d` / `0x144088e` | `0x861315/16` | afterburner key / brake key |
| `0xbe5bc4` / `0xbe5bc0` | `0x664c` / `0x6648` | afterburner active / brake active (int 0/1) |
| `0xbe6db4` | `0x783c` | boost countdown (s) |
| `0xbe5af6` | `0x657e` | "flight controls live" flag (cleared while docking or in a service beam) |
| `0xbe5c80` | `0x6708` | own ship type (index into the ship-type table) |
| `0xbe5b3e` | `0x65c6` | current location id |

Input path: the DirectInput polls (`0x49d1e0` keyboard; `0x499d00`/`0x49bb20`
other devices [inferred]) feed an action handler at `~0x49c85e` (switch
`0x49d048`). Digital keys step each stick by ±10 and clamp at ±100. Analog
devices write the value directly. Throttle `0x1440884` is set directly. While
`0xbe5aee` is set (text entry [inferred]), the main loop zeroes the three
sticks.

## 2. Per-frame order (in space)

From the WinMain loop, checkpoint ids in `0x14ff668`:

1. `0x403740` reshuffles the cells (top of frame), then dt = ΔQPC / freq.
2. Input polls (`0x499d00` / `0x49bb20` / `0x49d1e0`).
3. `0x428b60` power and energy bookkeeping (MODx-aware).
4. **`0x42a110` attitude**: Auto-Dock autopilot, stick rotation, afterburner and
   brake state, sector hazard damage.
5. **`0x429bb0` linear dynamics**: thrust, drag, brakes, boost, sector current.
6. Unless docking/docked (`0xbe5af4`, `0xbe5b01`): **`0x40d630` collisions and
   docking** (which first calls `0x40cb50`, the service-beam pad), then
   `0x40bf50` collisions for bots this client controls (§6, "Bots this client
   controls").
7. **`0x9c7270` position integration**, `pos += v·dt`, then the own ship is
   copied into contact slot 0 (`0x1042118`).
8. **`0x433010` dead reckoning for remote ships**, then `0x4338c0` bot AI.
9. Weapons (`0x439260`, `0x42ca30`, …), then the location-send timer `0x9ce1c0`.

The renderer (`0x9ede30`, a huge function Ghidra cuts short) only reads the
state. It draws the HUD speed, throttle and fuel gauges at `~0x9ff4ef`.

## 3. Linear dynamics: `0x429bb0`

```
throttle = min(throttle, 100);  thr = throttle * 0.01              // 0x14408b4
brake = 0
if throttle == 0 and speed > 0:                                     // brakes (needs brake active)
    brake = thrust * 0.08 / mass * brakeOn          ; ×2 if boost
    fuel -= engines * dt * 0.045 * brakeOn
speed    = |v|
dragDec  = speed² / mass * drag
if sector has a current, controls live, not in a gate's pull:     // see §7
    v += (current / mass) * drag * 100 * dt
if throttle > 0 or speed > 0:
    vhat = v / |v|
    dragDec += brake
    if afterburner: thr *= 1.3;  fuel -= engines * dt * (boost ? 0.2 : 0.16)
    if dragDec*dt <= speed or thr != 0:
        a = thrust * thr / mass ;  if boost and thr > 0: a *= 3
        if dt < 0.6:  v += (-dragDec * vhat + a * forward) * dt
    else:
        v = 0                         // drag would overshoot: stop dead
```

`engines` is the ship-type byte `+0x148` (number of engine mounts).
`boost` is `*0xbe5ad0`; its timer `0xbe6db4` runs down by dt.

Consequences:
- **Top speed** = √(thrust / drag), independent of mass. Mass only sets the
  acceleration. The HUD speed gauge uses √(1.3·thrust / drag) (afterburner top
  speed) as full scale (`0x9ff4ef`: `speed / sqrt(thrust*1.3/drag)`, clamped to 1).
- Drag always opposes the full velocity vector, so you can drift sideways
  and turning bleeds speed only through drag.
- The `dt < 0.6` guard skips thrust and drag on very long frames (e.g. after a
  stall), so velocity simply carries over.
- Velocity units ≈ units/s. The capture showed cruise ≈ 400 u/s (see
  reversing-map "Spatial check"), which is √(thrust/drag) for that ship.

## 4. Attitude: `0x42a110`

Kinematic rotation of the orthonormal frame (forward, up, right), one axis per
stick. The axes are re-orthogonalised with cross products and re-normalised
(`0x429b50`) after each step.

```
turnScale = (1.6 - thr) * 0.625            // 1.0 at idle, 0.375 at full, 0.1875 on afterburner
angle(stick, rate) = tan( sqrt|s| * s * rate * dt )     // signed s^1.5; deadzone |s| <= 1
yaw   (0x1440878): forward += angle(s, yawRate)*turnScale * right
pitch (0x144087c): forward += angle(s, pitchRate)*turnScale * up
roll  (0x1440880): up      += -angle(s, rollRate) * right        // roll NOT throttle-scaled
```

At full stick, sqrt(100)·100 = 1000, so the peak turn speed is about
**1000 × rate rad/s** (× turnScale for pitch/yaw). The same 1000 × rate × dt
figure is the per-frame cap used to smooth remote ships (§8). One yaw step
of this formula also appears in the launch handler `0x9cc750`.

The same function also handles:
- **Auto-Dock autopilot** (Auto-Dock MODx, `/dockoff`). It engages when the
  docking beacon is within **350** units (matches the item text "within 350
  meters"). State `0xbe5c6c` (`this+0x66f4`):
  - State 1: v and speed × 0.98 per frame until speed < 2.
  - State 2: steer the nose 0.023 per frame toward the beacon. While aligned
    (dot > 0.9999) and speed < 25, throttle goes up by 1 per frame (cap 22).
    If speed > 30, throttle drops to 0. Within 3 units it moves to state 3.
  - State 3: aim at the tube's inner end (`+0x7f4`). Throttle +1 while
    speed < 75, and 0 if speed > 90. Done within 60 units of it.
  - These steps are per frame, so they depend on frame rate.
  - The beacon is on the tube axis 1500 units out from `+0x814` (2550 in
    loc 107). Full write-up: **autodock.md**.
- **Afterburner** needs throttle ≥ 6 and fuel > 0. **Brakes** need throttle ≤ 5
  and fuel > 0. Running out shows a message and plays a sound. Brakes spawn
  particle effects.
- Sector hazard damage and gate kill zone (§7).
- First-person view modes (`this+0x6620`, `this+0x413100..`). These are camera
  only.

## 5. Mass, thrust and turn rates: `0x428290`

It is called on loadout changes (from cargo/equipment code, the launch handler
and the shop). `FUN_00439f70(id)` = "MODx id installed?".
- **mass** = hull mass (type `+0x30`) + per-slot equipment masses (engine,
  power plant, shield, radar, ECM, capacitor, …) + cargo (count × unit mass)
  + engines × engine mass + type `+0x150` [inferred: armour mass], plus 5000 in
  one special case. **Countermass MODx** then multiplies it: ×0.92 / ×0.89 /
  ×0.85 / ×0.80 (MODx 0x30/0x31/0x32/0x33). This matches the item texts
  (8 / 11 / 15 / 20 %).
- **thrust** = engines (type `+0x148`) × engine thrust (engine table at
  `0x146c880 + idx*0x10`, idx `0x140ac0c`). **Thrust MODx** add +3 / +5 / +8 %
  (0x10/0x14/0x18). **Power-distribution MODx** add +2 / +4 / +6 / +9 %
  (0x1c/0x1d/0x1e/0x1f). All of this matches the item texts.
- **drag, pitch, roll and yaw rates** are copied straight from the ship-type
  record.

### Ship-type table `0xff7eac + type*0x1e0`
**Loader: `0xa08440` `init_ship_types`.** There is no data file and no static
initialiser. The table is compiled into the code as about 7,400 `mov
[esi+disp], imm` stores, with esi = `0xbdf578`, so the record is at
this+`0x418934`. Names and class strings are `sprintf`'d into `+0x04` / `+0x18`.
`tools/jg_tables.py` re-extracts it to **`ship_types.csv`**. 88 types are
defined:
- 30–45 Solrain
- 60–75 Quantar
- 90–105 Octavius
- 196–199 TRI AI
- 220–225 faction defenders
- 230–259 conflux

The config screen (`0x46e9xx`, `0x4708xx`) displays these fields:

| off | type | meaning |
|---|---|---|
| `+0x04` | char[] | name |
| `+0x30` | f32 | hull mass (`config_mass`) |
| `+0x38` | f32 | drag (`config_drag`) |
| `+0x40` / `+0x44` / `+0x48` | f32 | pitch / roll / yaw rate (`config_pitch/roll/yaw`) |
| `+0x140` | i16 | cargo (`config_cargo`) |
| `+0x142` | i16 | spawn-offset length [inferred] |
| `+0x144` | f32 | ship size (diameter) used in docking clearance |
| `+0x148` | u8 | number of engines |
| `+0x14a..` | u8 | shield/radar/… class bytes |
| `+0x150` | i32 | armour (`config_armor`) |
| `+0x158/15c/160` | f32 | collision half-extents: right / up / forward |
| `+0x34` | f32 | unknown. Often 20,000–90,000 or equal to the mass [maybe max loaded mass]. |
| `+0x59..0x125` | 12-byte rows | per-slot equipment lists [not decoded] |

Peak full-stick turn rate ≈ 1000 × rate rad/s (§4). For example, the Premia
pitches at about 0.96 rad/s.

### Engine table `0x146c878 + idx*0x10` (27 engines)
**Loader: `0x414460` `init_item_tables`.** This one function hard-codes every
equipment category: the category names, then shields, engines, power plants
and so on. Each item's name goes to `0x14814d0 + idx*0x32`, its description
to `0x1488cbc + …`, and its stats as absolute `mov ds:addr, imm` stores.
Extracted to **`engines.csv`**.

| off | type | meaning |
|---|---|---|
| `+0` / `+1` | u8 | unknown. `+0` rises with tier (Money 0 … Dream 25, Tangent 31); `+1` takes values 0–5 [maybe manufacturer or faction]. |
| `+2` | u8 | 1 = sold, 0 = the pre-collapse PCE artifacts |
| `+4` | i16 | size (mount size 1–7) |
| `+6` | i16 | mass. Read in `0x428290` at `+0x88d306`. |
| `+8` | f32 | **thrust**. The UI shows thrust × 0.001 as "%.0fK" (`0x44e805`). |
| `+0xc` | f32 | efficiency. Power drawn = engines × thrust / efficiency, checked against the power plant (`0x146ca30 + idx*0xc`) in `0x46ffc1`. |

Examples:
- Money 758K
- Mitoria 720K
- Rush 3.2M at 0.65 efficiency
- PCE-5 3.8M at 0.95 efficiency (artifact)
- Tangent 16.65M, size 7, mass 32,500 (the "Roc" engine)
- Faction stock engines: S-E-15 745K, Mitoria-S 670K, Oteca 1.2M

**Top speed** follows from the drag law (§3): v_max = √(engines × thrust /
drag), and mass cancels out. `ship_types.csv` has a `vmax_per_sqrt_thrust`
column for this. Examples, before thrust and power MODx:

| ship | engines | v_max |
|---|---|---|
| Premia | 2 × S-E-15 | 320 |
| Premia | 2 × Respect | 460 |
| Interceptor | 2 × Rush | 600 |
| Storm | 2 × Oteca | 422 |
| Apteryx | 1 × Mitoria-S | 227 |
| Viceroy | 2 × Money | 112 |

Acceleration from rest is engines × thrust / mass. Mass includes equipment, so
acceleration is lower than hull mass alone suggests.

## 6. Collisions and docking: `0x40d630`

`0x40d630` is about 12 KB. Ghidra lumps it together with its tail up to
`0x410c9x`. Sibling `0x40ad30` handles player-built structures (table
`0x14c2f48`).

**Probe.** First a broad-phase test (`0x407b00`, model radius at `model+0x2b0`).
Then up to 7 segment-vs-model tests (`0x409a40`; it fills the contact point at
`0xb3377c..84`). The probe points are the ship centre and the centre ±½·extent
along the forward, up and right axes, swept by v·dt.

**Test primitives** [static, 2026-11-19]. Every collision, fuse and
line-of-fire check in the game is a **swept segment** p → p + d tested against
a model placed by a contact slot (`0x1042118 + slot·0x160c`: axes up/fwd/right
at `+0x68/+0x74/+0x80`, position `+0x15c8`). Nothing is volume-vs-volume.
- **Collision volumes are hand-authored and hard-coded in the exe**, not
  read from the model files and not computed. The `.x` files are plain
  render meshes: 708 of 718 have one `Mesh` and no frames or extra data.
  Each model object (`0x14ff690[id]`) carries 21 volume blocks of 0x40
  bytes: the whole model at `+0x2b0`, then 20 sub-volumes at
  `+0x2f0 + 0x40·i`. Block layout:
  - `+0x00` axis A, `+0x0c` axis B (unit vectors in model space; the third
    axis is their cross product),
  - `+0x18/1c/20` full extents,
  - `+0x24/28/2c` centre,
  - `+0x30` radius,
  - `+0x38` triangle-list head.

  The model loaders (`0xa27b30` and its siblings up to `≈0xa72250`, the
  generated "region-init" code) store axes, extents and centres as float
  immediates for **206 models**. 144 of those have whole-model bounds only;
  62 also have sub-volumes, e.g. station bodies 4/5/6 (9, 11, 7), 321–323
  (18, 13, 16), 381–383 (7, 8, 12), and player buildings 349–375 (2–7).
  Afterwards `0xa61f20` sets each radius to the largest |extent| and calls
  `0xa612d0` for every model.
  **Extracted** (2026-11-20) by `tools/jg_tables.py` into
  `extracts/collision_volumes.csv`: 440 blocks on 206 models (model, block
  0–20, axis A, axis B, extents, centre; same register tracking as the station
  points). `extracts/models.csv` maps model id → `.x` file (239 models) and, for
  26 models, a second mesh: **model `+0x934` is the station's tunnel mesh**
  (`*_tun.x`, stored e.g. at `0xa5f071`; also `beaconrings.x` for the
  beacon). `0xa612d0` walks that mesh too, appending its triangles to the
  same lists, so tunnels collide like the body.
- `0xa612d0` build_collision_tris. It walks the render mesh's 16-bit index
  buffer (32-byte vertices, position first). If sub-volume 0 is empty
  (`+0x308 == 0`), each triangle with **at least one vertex inside the
  whole-model volume** (`0x408130`) is copied into a 0x28-byte node:
  next, then x[3] @+4, y[3] @+0x10, z[3] @+0x1c in model space. The node is
  pushed onto `+0x2e8`. Otherwise the same test runs per non-empty
  sub-volume (**empty = extent x is 0**, `0xa61516`, so a sub-volume with a
  zero x extent is skipped even if the rest is set; model 414 block 2 is one),
  and the triangle goes onto that volume's list `+0x38`.
  A triangle can land in several lists, and one outside every volume is
  dropped. Models with no hard-coded volume probably have no collision
  triangles at all [inferred].
- `0x408130` point_in_volume(block, p) is an oriented-**ellipsoid** test:
  Σ 4·(dᵢ/extᵢ)² ≤ 1, where dᵢ is (p − centre) projected onto A, B and A×B.
  So the extents are diameters.
- `0x407b00` collide_bounds(block, slot, p, d) tests the segment against a
  volume placed by the slot. A cheap axis reject comes first, then
  |centre − p| ≤ radius + |d|, then closest approach, then the same
  ellipsoid test. It returns 1 on a hit.
- `0x409a40` collide_segment_vs_model(out, slot, p, d) runs the whole-model
  `0x407b00` test, then either the single list or, per sub-volume, that
  volume's `0x407b00` test and its triangles. Vertices are transformed by
  the slot axes and position and tested by `0x407430`. The nearest hit wins:
  `out[0]` = hit parameter, `out+0x14..1c` = contact point. If slot byte
  `+0x1583` is set, the triangles of model 119 are tested too. Model 119 is
  **`cargo.x`**, the towed cargo pod (`0xa587f0` loads it; `cargo_shield.x`
  sits beside it). The flag means "towing a pod". For remote players it
  comes from 0x05 gloc byte `+0x17` (`0x9cf8e7`, capture value 01). World
  objects with models 32/62/92 get it set (`0x4baa15`); those are the Cargo
  Tow hulls Pioneer, Condor and Thunder. So a tow's collision includes
  its pod.
- `0x407430` collide_segment_triangle is a **one-sided** segment-plane test
  followed by an edge test:
  ```
  n  = normalize((v1 - v0) x (v2 - v1))     // winding defines the front
  a  = n . (p - v0);      if a < 0: miss    // start must be in front
  b  = n . (p + d - v0);  if b > 0: miss    // end must be behind or on
  t  = -a / (n . d);  P = p + t*d
  for each edge (vi, vj): inside test using the edge vector, the edge
      midpoint and P against n               // 3 same-side checks
  ```
  Back faces never register, so a segment leaving a mesh from inside passes
  straight out.

**The mover is never a volume.** Every test is a zero-width segment (a ray
of length |v·dt|) against triangles. There is no model-vs-model, sphere or
box sweep. The movers are:
- **Own ship:** 7 points, the centre plus the centre ±½ of the type's
  collision extents (`+0x158/15c/160`) along right, up and forward. The ½ is
  the constant `0xac276c` (0.5) applied to each extent (`0x40aa1a`), so the
  ship-type extents are full sizes. Each
  point is swept by v·dt. A thin spar can slip between the points.
- **Bots I control:** the centre point only.
- **Missiles:** the centre point. Small models that don't get the mesh
  test (models outside 30–121) instead use the ellipsoid fuse (ai.md).

- `0x40a3f0` is the same test for a world-object table entry.

**Response to static objects and buildings** (same code in many copies):
```
n   = normalize(pos - contact)
pos += 0.1 * n
E   = 0.1 * mass * |v|^2                  // "impact energy" -> damage
v   = -0.8 * v + 5 * n                    // 80 % restitution plus a 5 u/s kick
shields -= E; overflow -> hull; hull < 0 -> death (0x9cec40)
```
If you die from an impact, the client sends a `0x0C` "impact" packet
(`"Failed to send impact packet"`, `0x40ea4c`, state link, 20 B:
`[0C][FE][u16 0xff2a92 @2][u32 pid @4]` + three dwords from locals, not
identified) and then calls `player_death(0)` (message-types "Combat"). The `0xbe6e30` counter accumulates
E × 1e-6.

**Mass-ratio "elastic" bounce: slots 1–5 and world objects only, not ships.**
*Corrected 2026-11-11.* An earlier version said this formula applies ship vs
ship. That is wrong. The formula is real, but it only runs in two loops, and
neither one visits the ship slots (10–199):
1. **Contact slots 1–5.** These are the gate slots plus slot 5. The loop is
   `esi = 1 … < 6` at `0x40f9ab` / `0x41062e`. Response:
   - `e = (m − m2)/(m + m2)`, using m2 = slot `+0x15c0`. Set on location
     change (`0x4ba9a3`): **50,000 for gate model 11; 500,000 for the beacon
     (2), asteroids (15–29) and huge rocks (305–319)**; other contact models
     get none (0, so e = 1 and they don't bounce you; this includes the pull
     gate 414) [static 2026-11-20]
   - `v' = v2 + (v − v2)·e`
   - `pos += 0.1·n`
   - damage E = 0.1·m·|Δv|² to shields, then hull
   - debug print `"Mass F1 %f\n"` (`0xac2a20`)

   Gates have v2 = 0, so this is effectively a bounce off a fixed body.
2. **The sector world-object table** at `0x140ad48 + i*0xdc`, 1000 entries.
   Each entry holds a model at `+0`, mass at `+0x58` and active at `+0xa8`.
   **Every entry's mass is 500,000** (`0x4badb8`, stored as an integer);
   the velocity at `+0x84` isn't set there (0).
   The table is filled on location change by `0x4ba420` and covers asteroids,
   debris and the like. Entry 969 is skipped; it is a special model-`0x1ad`
   object set up by `0x9c99ef`. Tests are `0x4083f0` / `0x40a3f0`. Response:
   - `v' = e·v + 5·n`
   - the same damage formula

   So rocks and junk bounce you and hurt.

**Other ships (players and bots) get only a soft separation push** (`0x410659`).
There is no bounce and no damage. The push applies to every slot 10–199 that
is active, has `+3 == 0xff`, has model < `0x154`, and is within
`(size_a + size_b)·5/12`, where size is the model's `+0x2e0`, measured by slot
`+0x94` distance. It runs only when not docked (`0xbe5bc8`) and when no slot
1–5 contact happened this frame. Per frame:
```
n    = normalize(other.pos - my.pos)
v   -= n * 4 * dt * m_other/(m_me + m_other)     // heavier other ship pushes harder
pos -= 0.05 * n
seal(1)
```
At 60 fps that is only 4 u/s² scaled by the mass share, so a ship moving at
any real speed passes through another ship and gets nudged slightly. Only
your own ship is moved; each client pushes itself. In practice, then, **there
are no collisions between player ships**, which matches how the game is
remembered. Remote ships are not run through the static-object probe either
(§8).

**Bots this client controls: `0x40bf50`** [static, 2026-11-19; an earlier
version called this "avoidance", which is wrong]. For each pbotloc entry whose
controller (`+0xc`) is me, the bot's slot is swept by v·dt (`[0x1440914]` =
dt) with `0x409a40`. The search stops at the first contact, in this order:
1. **Ship slots 10–199**: active, `+3 == 0xff`, not the bot itself. This is
   a full mesh test, so **bots collide with players and with other bots**.
   The sweep uses the bot's velocity relative to the other ship.
2. **Station slot 6**, if active. When the bot is inside the station's
   bounds, the code works out where it sits relative to the docking axis
   (model `+0x7f4` → `+0x814`, constants 761, 449.44, 175). A bot lined up
   on the approach is not tested against the station, so docking defenders
   can fly in [inferred from the constants and branch structure].
3. **Special objects, slots 300–349.**
4. **Player structures**, 64 entries (`0x1185e1c + i·0x160c`, within 1500
   units), tested as slot 235 + i. A miss calls `0x40a650` [role unknown].
5. **Slots 0–5**: gates and the beacon.
6. Separately, **world objects 0–999** (asteroids etc., `0x40a3f0`).

Response, which is the same for ships and world objects:
```
e  = (m_bot - m_other) / (m_bot + m_other)   // m_bot = ship-type mass (0xff7ee0 + type*0x1e0)
                                             // m_other = slot +0x15c0, or world object +0x58
v' = v_other + (v - v_other) * e             // world objects carry a velocity at +0x84
E  = 0.1 * m_bot * |v' - v|^2
shields(+0x15f8) -= E    // a positive shield sets the hit flash: +0x2f = 1, +0xc8 = clock
overflow -> hull(+0x1600), shields = 0
hull < 0 and slot +2 == 0 -> botloc death report 0x9ce490(slot, me, -1)
```
Unlike the own ship, a bot gets **no normal, no push-out and no +5 kick**.
The velocity is scaled toward the other body's velocity along the bot's own
direction of travel, so a bot moving fast enough can still end up inside the
mesh on later frames. Only the bot changes. The ship it hit (even a player)
is untouched on this client, and that player's own client gives them only
the soft push above. **So a bot that rams a player bounces and takes damage,
and the player only gets nudged.**

**Summary: who collides with what on one client.**
| mover | vs static/world objects | vs ships | test |
|---|---|---|---|
| own ship | bounce `−0.8v + 5n` + damage | soft push only, no damage | 7-point probe + mesh |
| bots I control | mass-ratio bounce + damage, no push-out | mesh hit, mass-ratio bounce + damage (bot only) | centre segment + mesh |
| remote players, bots others control | none | none | none: dead reckoning, gate pull, repulsor only (§8) |
| own missiles | fuse/impact | fuse (contact or proximity) | segment + mesh/ellipsoid (ai.md) |
| others' missiles | none locally | none | slots only |

The server does no collision on the wire either. No message carries a
collision. A death by impact sends C→S `0x0C`, and a bot death goes in
botloc.

**Special objects in contact slots 1–4 (gates) and 300–349** (slot model id at
`ship+0x34`):
| model | effect |
|---|---|
| `0x19e` (414, `x_ai_gate`, the active jumpgate) | **Pull** for 1 < d < 400: `v -= (10000/m)·(1 − cos(d·π/200))·r̂` (r̂ points away from the gate centre). Applied **per frame, not × dt**. **Gate burn:** after loitering within 375 units for more than 17.99 s, shields/hull take `(400−d)·0.0025·(ln(1+t')+t')·dt·1e7 / (110−t')` per frame, with t' = t − 17.99. |
| `0x19a` (410) | Excluded from the pull. Within 129.5 units you die instantly (`0x42a110` tail). `0x9cc750` puts this model into flagged gate slots. The model is `cf_infest.x`, so this is probably a conflux-infested gate (2026-11-18; earlier guess "closed gate"). |
| `0x130` (304) | On contact, damage = (1 / distance)·dt·1e8 per frame [inferred: sun/hazard body]. |
| `0x1ae` (430, `cf_eruption.x`, placed by S→C 0x78 type 15) | **Repulsor** within 2000: `v += r̂·(10000/m)·(1 + 1/d)` per frame (not × dt). |

**Docking with stations** (model points `+0x7f4` → `+0x814` define the docking
axis; the four launch tubes are at `+0x834 + k*0x20` → `+0x8b4 + k*0x20`,
see launch.md):
- **Dock** when all of these hold: inside the tube (radial² + (size/2)² < 761);
  less than **60** units along the axis; **speed < 100**; controls live; you
  are allowed to dock there (faction table `0xbe5abc[owner]` == 1, location not
  "unregulated" (6), `0xbe5aeb` clear). Docking zeroes v and all sticks and the
  throttle, sends `0x0F` (392 B loadout) and `0x07` sub 2 "dock" (`"Error
  Sending Dock Packet"`), and sets `0xbe5bc8`/`0xbe5b01`.
- **Otherwise**, inside a launch tube (and only while the "just launched"
  flag `0xbe5af6` is set) you are pushed out along it by 3 u/s per frame
  (3.3 for player stations, whose thresholds are 1250 / 1761 within a 1500
  range). This is the launch catapult (launch.md). Earlier versions of these
  notes read it as guide segments pushing off-tube ships; corrected
  2026-11-20.

**Station points extract** [static + capture check, 2026-11-19]. The dock
and launch points are float immediates in the same model loaders as the
collision volumes. `tools/jg_tables.py` tracks register immediates
(`mov`/`xor`), maps a register to a model through `mov reg, ds:[0x14ff690 +
id·4]`, and collects stores to `+0x7f4..+0x91c`. It writes
**`extracts/station_points.csv`**: 25 station models, each with `dock_start`
(`+0x7f4`), `dock_end` (`+0x814`) and launch tubes `tube0..3_in`
(`+0x834 + 0x20k`) / `tube0..3_out` (`+0x8b4 + 0x20k`), in model space.
Components that are never stored are written as 0 [inferred: the object is
zeroed beforehand]. Before 2026-11-20 the extractor missed model pointers
loaded into eax (objdump prints `mov eax,ds:…` without `DWORD PTR`), which
zeroed some components (e.g. Quantar Core tube 0's inner x, and dock points
of models 4–8, 347, 367, 412, 413) and dropped model 411. Placement: the sector's contact
slot 6 (station row, `object_id` 6, in `sector_objects_all.csv`). World point =
pos + x·right + y·up + z·fwd, with right = up × fwd. Checks against the captures:
- Launches: all three captured launches (118 tube 2, 109 tube 2, 107
  tube 0) first report at rest exactly where launch.md's placement rule puts
  them (within 1 unit).
- Quantar Core (107, model 323, station at (−23.37, 0, 0), identity
  orientation): the last report before docking, (−23.0, 146.8, 10.5), is 59 units
  past `dock_end` (−20.2, 143.5, −48.5) on the tube axis. So "within 60
  along the axis" is measured from `dock_end`, and ships come in from outside
  along `dock_start → dock_end`.
- Quantar Depot (118, model 380): the captured launch y (611.3) equals the
  tubes' y.

**Service beam `0x40cb50`** (refuel / repair / rearm / transaction pads, models
`0x157..0x177`): it freezes the ship (v = 0, sticks = 0) and shows "Hold
position to complete transaction". The transaction packet `0x79` goes out.
After 14 s it pushes you out at 10 u/s² along the pad axis.

## 7. Sector fields

Nothing in the static data sets these per location: the location initialiser
only zeroes loc 0's (`0x4a8d28`), and no indexed writer was found. They must
arrive at runtime [inferred: from a server message; not identified]. So the
extracts can't say which sectors have currents.

The location struct `0xfd9220 + loc*100` gains three physics fields:
- `+0x4c` byte: the sector has a field.
- `+0x4d` byte: field kind. 7 = damaging.
- `+0x54` f32: strength. `+0x58/5c/60` f32: vector.

When the field is present, controls are live, and you are not in a gate's
pull (`0xbe5bb4`/`0xbe5bb8` clear), `0x429bb0` adds a **current**:
`v += (vec / mass)·drag·100·dt`.
For kind 7, `0x42a110` drains **hull** by `(strength / hullScale)·dt·3e8` and
kills you below 0.
`0x9dc570` switches the space-dust effect to streaming along v in these
sectors (visual only).

## 8. Other ships

**Remote players and bots I don't control** (`0x433010`). This covers every
active contact slot with `+3 == 0xff`, not dead, and model < 0x103:
- Orientation slews (`0x42c540`) toward the networked forward/up vectors
  (`+0x15d4`, `+0x15e0`). The per-frame step is capped at the type's
  yaw/pitch/roll rate × dt × 1000.
- Velocity follows **the same thrust/drag law**. It uses slot mass (`+0x15c0`,
  int), type drag, and slot thrust `+0x1608` (= thrust × throttle, as each
  client publishes for itself in `0x9c7270`), pushing along the slot's forward
  vector. Zero thrust means drag only, and the ship stops when drag·dt > speed.
- Gate pull (`0x19e`) and repulsor (`0x1ae`) apply as for the own ship.
- `pos += v·dt`.
- Other active slots get plain linear extrapolation, `pos += v·dt`.
- An extra entity at `0x140abb0..ac08` (type, mass, thrust, forward, velocity,
  position) uses the same law. It is parked at (−1e5, 1e5, 1e5) on sector entry
  [role unknown].

### Network snapshots vs the local simulation  [static, 2026-11-11]
**There is no separate "displayed" and "true" position.** Each remote ship has
one position (`+0x15c8`) and one velocity (`+0x15ec`). They are dead-reckoned
every frame (above) and corrected when a packet arrives. Orientation is the
only state kept in two copies. The networked forward and up vectors
(`+0x15d4` / `+0x15e0`) are snapped from each packet, and the displayed
axes (`+0x68` / `+0x74` / `+0x80`) slew toward them at the type's turn rate.

There are three S→C handlers, all in dispatcher `0x9cf6b0`. Each one is the
server relaying another client's own packet, with the same layout as C→S;
see message-types.md:

| msg | handler | carries |
|---|---|---|
| `0x05` gloc | `0x9cf7c8` | full state: position @0x90, velocity i16 ×0.1 @0x54, fwd/up i16 ×1e-4, thrust, loadout, names |
| `0x33` small loc | `0x9d573c` | position @0x1c, **velocity** i16 ×0.1 @0x12, fwd/up @6/@0xc, thrust @0x28, clock @0x34 |
| `0x34` mini loc | `0x9d5a9d` | position @0x18, fwd/up, thrust @0x24, clock @0x30. No velocity. |

The packet is matched to a slot by player id: packet `0x18` / `0x14` / `0x5c`
against slot `+0x15b4`. The slot must be active, have `+3 == 0xff` and not be
dead. Then:
1. **Ordering.** The packet clock must be newer than slot `+0x15bc`;
   otherwise it is dropped. For 0x05 the log says `"throwing away old loc2
   packet!"`.
2. old = slot position (the current dead-reckoned value). Copy the packet
   position into the slot; 0x33 and 0x05 also overwrite velocity.
3. Δ = new − old. **If |Δ|² < 40000 (error < 200 units): put the position back
   to old and add 0.1·Δ to velocity.** Otherwise keep the new position, a hard
   snap.
4. Store the new fwd/up, effective thrust (`+0x1608`) and clock. The
   engine-glow byte `+0x156d` is 0 when thrust is 0, otherwise 50.

So small errors are never applied to the position directly. They become a
velocity nudge, which dead reckoning integrates over the next frames, and
drag partly damps the extra velocity. Large errors (≥ 200 units, e.g. after a
gate jump or packet loss) teleport the ship. No latency compensation is done:
the packet clock is used only for ordering. A remote ship is therefore shown
where its owner was about one-way-latency ago, plus whatever extrapolation
has run since. When 0x33/0x34 stop (no one within 10,000 units), remote ships
coast on dead reckoning until the next 0x05, which arrives every 5–13.6 s.

**How the blend behaves over time** [reasoned from the code, not measured]:
- **It decays exponentially, with a time constant of about 10 s.** Each packet
  re-measures Δ against the drifted dead-reckoned position and nudges
  velocity by 0.1·Δ, so dΔ/dt ≈ −0.1·Δ. About 63 % of an error is gone after
  10 s and about 95 % after 30 s. Errors under 200 u can persist for a long
  time.
- **0x33 and 0x05 replace the nudge; 0x34 stacks on it.** 0x33 and 0x05
  overwrite velocity, so v becomes packet velocity + 0.1·Δ and the old nudge
  is discarded. 0x34 has no velocity field and only does `v += 0.1·Δ`, so
  nudges build up across a 0x34 tick and are cleared at the next 0x33.
- **Drag erodes the nudge.** Dead reckoning applies the thrust/drag law to the
  whole velocity. Near top speed, thrust and drag balance, and the excess
  decays quickly; a linearised estimate for a Premia at about 320 u/s gives
  roughly 1 s. Fast ships therefore converge more slowly than 10 s. At low
  speed, drag ∝ v² barely touches the nudge.
- **The target is stale.** There is no latency compensation, so the ship
  converges toward a track delayed by about one-way latency. It can carry up
  to about 200 u of standing error before a snap.

**Bots controlled by someone else** (S→C `0x71` → `0x9d6f25`) are a hard
snap. Position, velocity (i16 ×0.1) and orientation (i16 × `0xaeae10`) are
written straight into both the networked *and* the displayed axes, with no
blend.

**Contact slot layout** (`0x1042118 + i*0x160c`), additions to reversing-map:
| off | meaning |
|---|---|
| `+0x34` | model id |
| `+0x156d` | engine-glow / thrusting byte (0 or 50) |
| `+0x15b4` | player id (matched against relayed packets) |
| `+0x15bc` | clock of last applied location packet (ordering) |
| `+0x68/+0x74/+0x80` | displayed up / forward / right |
| `+0x94` | distance to me |
| `+0x15c0` | mass (int) |
| **`+0x15c8/+0x15cc/+0x15d0`** | **position** X/Y/Z (verified: `0x9c7270`, all distance code) |
| `+0x15d4` / `+0x15e0` | networked forward / up |
| `+0x15ec/+0x15f0/+0x15f4` | velocity |
| `+0x1608` | effective thrust |

> Conflict: reversing-map.md's conflux table lists "+0x15c8/+0x15cc hull". In
> the physics code those offsets are the position. Hull is likely elsewhere;
> re-check that entry.

**Bots I control** run their own AI and steering code (`0x4338c0`;
reversing-map "Conflux"), and collide in `0x40bf50` (§6). Their integrator was
not traced here.

## 9. Anti-tamper around the physics

1. **Cell shuffling** every frame (§1).
2. **Velocity seal** `0xa1a700(arg)`. It computes `chk = vx + K − vz + vy`,
   where K = 26.2879 (double at `0xb0a690`). With arg = 1 it stores chk as a
   double at `0xbdf3e8`; every legitimate velocity write is followed by
   `a1a700(1)`. With any other arg it compares and, on a mismatch, sets
   checkpoint `0x3fd` and calls **`0xa215e0`, the shutdown routine**. That
   routine logs "Last Code: %d", sends the logout message and writes
   `error.txt`. So a velocity edit that the check catches **closes the
   client** with Last Code 1021. Which call sites verify (rather than re-seal)
   is only partly mapped: most pass the argument in a register.
3. A speed tripwire `0x403120` runs every frame (`0xa2666c`, gated on
   `0xbe5b90`, `!0xbe5b08`, `!0xbe3d75`):
   if speed > **1225**, it calls `0x440660(1)` and sets `0xb8e288 = 2`
   **[confirmed 2026-11-17]** `0xb8e288 == 2` makes the main loop (`0xa27269`) set
   checkpoint `0x3fc` and call the shutdown `0xa215e0`: a clean logout and exit
   that writes `error_nocrash.txt`. `0x440660` only writes spaces/newline to a
   log. Hit live with the `--dev-drag 92=0.05` patch: the launch-tube push takes
   a low-drag ship past 1225 and the client exits. The constant (f32 `0xac2620`)
   has this one reader, and `.rdata` is writable in this binary (0xc0000040).
4. **Table checksums** `0xa19400` [static + live, 2026-11-17]. The main loop
   calls it in flight (`0xa27255`: checkpoint 32 when `0xbe5b90` is set, then
   `call 0xa19400`). It sums bytes as `Σ sbyte[i] << (16·(i mod 3))` (a shift by
   32 wraps to 0) into 64 bits and compares with a hard-coded constant. On a
   mismatch it sets a checkpoint and calls the shutdown `0xa215e0`. Covered:
   - Equipment tables, byte loops: `0x146c878..ca28` (includes the **engine
     table** `0x146c880`, cmp `0x3afe75` @`0xa1964a`), `0x146ca28..cb6c`,
     `0x146cb6c..ccac`, `0x146ccac..cd0c`, `0x146cd0c..cf64`,
     `0x146cf64..d0b4`, `0x146d0b4..d2e4` (cmp `0x199e201d` @`0xa19a2a`), and
     `0x14fcbd4..d054`.
   - **Ship-type table**: mass, drag, pitch/roll/yaw and +0x140 of every player
     hull (types 30–40, 45, 60–71, 75, 90–101, 105) are copied into a 0x360-byte
     buffer, cmp `0x9db03af` @`0xa1a5a2` → **Last Code 117**. Hit live by
     `--dev-drag 92=0.25`.
   So ship or equipment stats can't be patched in memory, and the expected
   totals live in `.text` (0x60000020, not writable; the stock Msb32 imports no
   VirtualProtect), so they can't be adjusted over 0xFE either. Note also that
   the 0xFE reaches the game dispatcher as well, which logs "Received a packet
   of unknown type 254".
   [inferred: autopilot/cruise or anti-speedhack reaction; not traced].
5. **Game-file checksums** [static + live, 2026-11-18]. The exe carries
   two tables of `{char name[0x20] (space padded); u64 sum}` (0x28 bytes):
   `0xb1e098..0xb232f0`, 527 `.bmp` (textures), and `0xb232f0..0xb296a0`, 638
   `.x` (models). Both are in `.data`. `0x407050(name)` sums the file like
   item 4 (`Σ sbyte[i] << 16·(i mod 3)`, 64-bit). Two quirks matter if you
   want to reproduce it:
   - The file is opened in MSVC text mode (mode 1, no binary), so CRLF reads
     as LF and a 0x1A byte ends the file.
   - The loop reads one byte, adds it, then tests EOF, so the failed read at
     EOF adds the last byte again at index len.

   `tools/jg_integrity.py` reproduces it and matches 1160 of 1165 stock files.
   - **At startup** (`0xa24dc8`): `0x407130` checks every texture and logs
     `D3D_ALLOC_ERROR 100%d` (the table index) for each mismatch.
     `0x4071f0` does the same for the models with `101%d`. If any file fails,
     the client logs `D3D_ALLOC_ERROR 100` (or `101`) and calls the shutdown
     `0xa215e0`. A missing file also fails (the sum stays 0).
     Live 2026-11-18: replacing ast1–ast5.bmp with the older 128×128 8-bit
     versions logged `D3D_ALLOC_ERROR 1002`…`1006` (indices 2–6 = AST1–AST5)
     and the client exited at startup.
   - **In flight**: on each sector entry (`0xa25b14`) the client picks a random
     start index 0–29 and arms `0xb2a700`. Every 15 s (f32 `0xaeae68`) the main
     loop re-checks one texture entry, `0x407190(idx)`, wrapping 30 → 0, until
     it is back at the start. Only the first 31 texture entries are covered
     (SPLASH24, AST0–6, AA/BB/CC0x, ALIEN01, CONFLUX1–3, AN01). A mismatch sets
     `0xb8e288 = 2`, which is the item 3 exit with Last Code 1020. So swapping
     a file after startup still gets caught.

   To run modified textures or models, the matching table sum has to change.
   It is not a server matter: the startup check runs before any connection.
   (A patch is to return early, `31 C0 C3` at file offsets 0x7130/0x7190/0x71f0,
   which disables the check; nothing else reads the table or checksums the
   exe's code. Our approach to modded assets: ../docs/client-mods.md.)

6. **Loadout seal** [static, 2026-11-19]. `0x9cb650` loadout_seal sums the
   fitted-item id bytes into `0xba79c0` (a plain byte sum, not the shifted
   one):
   - gun slots `0xff7d00..07`,
   - the next 8 slots `0xff7d08..0f`,
   - engine `0x140ac0c`, plus powerplant/radar/ECM/shield/capacitor, read as
     `0xff7d19..1d` and mirrored into `0x140ac0d..11`,
   - `0x140ac1d..24`.

   It then tail-jumps to `0x9c8de0`. It is called wherever the loadout
   legitimately changes: `0x4374f8` (a slot emptied, `0xfe`), `0x9cd1a4`,
   `0x9cd3de`, `0x9d24f1` (a message handler), and `0xa1bf9e`.
   `0x9c8f40` loadout_check runs every frame from the main loop (`0xa27264`,
   just before the `0xb8e288` exit test). It re-copies the mirror bytes,
   recomputes the sum and, on a mismatch, sets checkpoint `0x6b` and calls
   the shutdown → **Last Code 107**. It is skipped while `0xbe5aeb` is set.
   So equipment ids can't be poked in memory, but the stored sum sits in
   writable `.data`, so this is obfuscation rather than protection.
7. **Mayday rate limit** [static, 2026-11-19]. Mayday has two
   key-bound actions, "Faction Mayday" and "Squad Mayday" (samples.txt; fixes.txt: defaults "=" for faction and "-" for squad, set
   in Options → Controls:Combat). They are key actions 44 and 45
   (`0x49c872` switch → `0x1440899` / `0x144089a`), handled in
   `0x43d6d0`:
   - Faction sends C→S `0xC3` sub `0xF6` ("mayday1").
   - Squad sends `0xC3` sub `0xF5` ("mayday2") and needs a squad name
     (`0xbe3da4`); otherwise the client prints "Only available to members of
     a squad".

   Both need in-flight (`0xbe5b90`), not the simulator (`0xbe5aeb`), and a
   location 101–349 (space). Both are 2-byte packets. The server must add the
   position and broadcast.

   They share a counter at `0x14ff678` with `/tour` (`0x4a003f`, `0xC3`
   `0xF2`). It is reset to 0 only on location change (`0xa25d6b`). If the
   counter is 0, the press sends and the counter becomes 1. **So you get one
   mayday (or tour) per sector visit.** A further press prints "Message not
   sent due to overuse of command." and increments the counter. Past 7 it
   warns "ALERT: Continued overuse will result in disconnection." Past 10,
   `0x43d6d0` returns 0, and the main loop (`0xa26a33`, checkpoint 0x25)
   logs **"Exit: mayday function abuse"** and shuts down. This is anti-spam
   rather than anti-cheat, but it is the only other client-side self-kick.

**Checked and not present** (2026-11-19). The import table has no
IsDebuggerPresent, CheckRemoteDebuggerPresent, Toolhelp/Process32,
FindWindow/EnumWindows, ReadProcessMemory or OpenProcess, and no strings for
those names, cheat tools or debuggers, so nothing resolves them at runtime
either. `VirtualProtect`, `GetTickCount`, `TerminateProcess` and
`CreateProcessA` each have one call site, all in CRT/library code
(`0xaa…`–`0xab…`; CreateProcess runs `dxdiag.exe` after a video-mode
failure). `timeGetTime` drives the FPS counter (`0xa20fe6`) and the network
library. `QueryPerformanceCounter` drives frame timing and random seeding
(`0x9c8cab`). Nothing compares the two clocks, so there is **no speed-hack
(clock-skew) check**. There is no code self-checksum (item 5).

Every call to the shutdown `0xa215e0` (25 sites), and every write of
`0xb8e288 = 2`, is now accounted for:
- the anti-tamper items 2–7;
- init and video failures (`0xa248d7`, `0xa25a68`, `0xa25c1c`, `0xa25c5c`);
- `/quit`-style commands (`0x4a72eb`);
- the in-flight quit button (`0x48f25d`). It compares shield/hull with
  values snapshotted at `0x440d27`/`0x440d43` before allowing it, possibly a
  combat-logout guard [not traced];
- "Network Problems Detected" (`0x482724`);
- the menu quit (`0x49128f`);
- the crash path (`0xa2734e`);
- the Dpnet.dll warning (`0x4420e2`, which sets 1, not 2).
8. Velocity does not go into the frequent packets: `0x33` carries
   **velocity × 10 as i16** at `@0x12` (from snapshot `0xff2ad0..d8`, scale
   const `0xb05ac8` = 10.0). Orientation goes as i16 × 10000. So the server
   *could* sanity-check speed. Whether it does is unknown (server-side).

Taken together, the protections are local obfuscation and detection only.
Since the client is authoritative for its own movement, any real enforcement
would have to happen on the server.

## 10. Function names added

`0x403150` cells_alloc · `0x403740` cells_shuffle_per_frame · `0x403120`
speed_tripwire · `0x428290` recalc_mass_thrust · `0x428b60` power_tick ·
`0x429bb0` flight_linear_dynamics · `0x42a110` flight_attitude_autodock ·
`0x40d630` own_collisions_docking · `0x40ad30` collide_player_structures ·
`0x40cb50` service_beam_pad · `0x9c7270` integrate_position_publish_slot0 ·
`0x433010` remote_ships_dead_reckon · `0x42c540` remote_orient_slew ·
`0xa1a700` velocity_seal · `0xa215e0` shutdown_error_exit · `0x9cc750`
enter_space_handler · `0x4bbfb0` freecam_tick · `0x9dc570` space_dust_fx ·
`0x49d1e0` dinput_keyboard_poll · `0xa08440` init_ship_types ·
`0x414460` init_item_tables · `0x4083f0` collide_broadphase_worldobj ·
`0x40a3f0` collide_segment_vs_worldobj · `0x407b00` collide_bounds ·
`0x407430` collide_segment_triangle · `0x40bf50` bots_owned_collision_tick.

## Open items
- Which `0xa1a700` call sites verify vs re-seal, and what `0x440660` does.
- ~~Ship-type table loader~~: done. Hard-coded in `0xa08440`; engines in
  `0x414460` (`ship_types.csv`, `engines.csv`). Still open: ship `+0x34`, the
  `+0x59..` slot lists, and engine bytes `+0`/`+1`.
- ~~Contact slot 5: what object it holds~~ — the sector **beacon** (model 2); the 0x61 beacon handler `0x9d67d5` requires it. [static, 2026-11-17]
- Runtime check of the 200-unit snap / 0.1·Δ velocity blend: compare a remote
  ship's slot position with relayed 0x33 positions in a two-client capture.
- Bot steering/integration (`0x4338c0`, `0x42f360`), missile flight
  (`0x42ca30`, the `fptan` sites at `0x42af2f..0x42efd0`).
- Runtime confirmation: hook `0x429bb0` (or read the cells through the pointer
  table) and compare with the 0x33 velocity field in a capture.
