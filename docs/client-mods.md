# Client modifications we use

The classic client's integrity checks (game-file checksum tables, loadout
seal, table checksums) are documented in ../notes/physics.md §9. This file
covers what we do about them.

## Modded assets: re-sealed exe  [2026-11-18]

To run modified textures or models, the matching table sum has to change.

- **Chosen approach:** `../notes/tools/jg_reseal.py <install dir>` writes
  `Jumpgate_mod.exe`, a copy whose table sums match the files on disk.
  - It changes only the 8-byte sum fields and leaves the check code active, so
    untouched assets are still verified.
  - The original exe is never modified.
- **Rejected:** the early-return patch (`31 C0 C3` at file offsets
  0x7130/0x7190/0x71f0). It would disable the check entirely.
- **Live state 2026-11-18:**
  - ast1–5.bmp swapped for the 128×128 8-bit versions; the originals are in
    `../notes/orig-assets/`.
  - 5 entries re-sealed; `jg_integrity.py --exe Jumpgate_mod.exe` reports 0
    failures.
  - The client run itself is not yet confirmed.
- **Use the modded copy with our server only.**
