"""Write Jumpgate_mod.exe: a copy of Jumpgate.exe whose file-integrity table
matches the textures/models currently in the install directory.

  python3 tools/jg_reseal.py /path/to/Jumpgate [--out Jumpgate_mod.exe] [--dry-run]

- Reads <dir>/Jumpgate.exe (never modified) and every file its table names.
- For each file whose checksum differs, writes the new u64 sum into that entry
  of the copy. Nothing else in the exe changes; the check code stays active,
  so all the untouched assets are still verified.
- Missing files are left alone (and still fail the check), so they get noticed.
- Re-run after changing assets again. It always starts from the original
  Jumpgate.exe, so the output only ever differs from it in sum fields.

Use the modded exe only with your own server, never with jumpgate-tri.org.
Layout and checksum: tools/jg_integrity.py, physics.md section 9 item 5.
"""
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from jg_integrity import PE, cks, entries, game_files  # noqa: E402

ORIG_SHA256 = 'fef6e50fc4608c1514242932823fb8cbeeda9ac6217d9f819fd802d94ca7bdf8'


def main(argv):
    import hashlib
    args = [a for a in argv if not a.startswith('--')]
    out_name = 'Jumpgate_mod.exe'
    if '--out' in argv:
        out_name = argv[argv.index('--out') + 1]
        args.remove(out_name)
    dry = '--dry-run' in argv
    game = args[0] if args else '.'
    src = os.path.join(game, 'Jumpgate.exe')
    out = out_name if os.path.isabs(out_name) else os.path.join(game, out_name)
    if os.path.abspath(out) == os.path.abspath(src):
        sys.exit('refusing to overwrite Jumpgate.exe; pick another --out name')

    data = open(src, 'rb').read()
    if hashlib.sha256(data).hexdigest() != ORIG_SHA256:
        sys.exit(f'{src} is not the expected 2.0127 build (SHA-256 mismatch); '
                 'table addresses would be wrong')
    pe = PE(data)
    buf = bytearray(data)
    files = game_files(game)
    changed, missing = [], []
    for code, i, name, exp, sum_off in entries(pe):
        p = files.get(name.lower())
        if not p:
            missing.append(name)
            continue
        got = cks(open(p, 'rb').read())
        if got != exp:
            struct.pack_into('<Q', buf, sum_off, got)
            changed.append((code, i, name, exp, got))

    for code, i, name, exp, got in changed:
        print(f'  re-sealed {name:24} (entry {code}{i}) {exp:016x} -> {got:016x}')
    if missing:
        print(f'  {len(missing)} table file(s) missing, left as is: {", ".join(missing[:8])}'
              + (' ...' if len(missing) > 8 else ''))
    if not changed:
        print('all present files already match the original table; nothing to re-seal')
    if dry:
        print('dry run: nothing written')
        return 0
    with open(out, 'wb') as f:
        f.write(buf)
    print(f'wrote {out} ({len(changed)} entr{"y" if len(changed) == 1 else "ies"} changed)')
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
