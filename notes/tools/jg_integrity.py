"""Check game files against Jumpgate.exe's built-in file-integrity table.

  python3 tools/jg_integrity.py /path/to/Jumpgate [--exe Jumpgate_mod.exe]

Two tables of 0x28-byte entries {char name[0x20] (space padded); u64 sum}:
  0xb1e098..0xb232f0  527 .bmp  -> error "D3D_ALLOC_ERROR 100<index>"
  0xb232f0..0xb296a0  638 .x    -> error "D3D_ALLOC_ERROR 101<index>"
Checksum 0x407050: file opened in MSVC *text* mode (CRLF->LF, stops at 0x1A),
read 1 byte at a time; sum += sbyte << (16*(i%3)) in 64 bits; the failed read
at EOF adds the last byte once more at index len. See physics.md section 9 item 5.
Only the paths the table names are read (top level of the install).
--exe checks a different exe (default <dir>/Jumpgate.exe), e.g. a re-sealed copy
from tools/jg_reseal.py.
"""
import os
import struct
import sys

TABLES = [(0xb1e098, 0xb232f0, '100'), (0xb232f0, 0xb296a0, '101')]
ENTRY = 0x28


class PE:
    def __init__(self, data):
        self.b = data
        pe = struct.unpack_from('<I', data, 0x3c)[0]
        n = struct.unpack_from('<H', data, pe + 6)[0]
        opt = struct.unpack_from('<H', data, pe + 20)[0]
        self.ib = struct.unpack_from('<I', data, pe + 24 + 28)[0]
        self.secs = []
        for i in range(n):
            o = pe + 24 + opt + 40 * i
            _vs, va, rs, ra = struct.unpack_from('<IIII', data, o + 8)
            self.secs.append((va, rs, ra))

    def off(self, v):
        v -= self.ib
        for va, rs, ra in self.secs:
            if va <= v < va + rs:
                return ra + v - va
        raise ValueError(hex(v + self.ib))


def textmode(d):
    z = d.find(b'\x1a')
    if z >= 0:
        d = d[:z]
    return d.replace(b'\r\n', b'\n')


def cks(d, text=True, stale=True):
    """The client's checksum of file bytes d (defaults reproduce 0x407050)."""
    if text:
        d = textmode(d)
    s = 0
    for i, c in enumerate(d):
        if c >= 128:
            c -= 256
        s += c << (16 * (i % 3))
    if stale and d:
        c = d[-1]
        if c >= 128:
            c -= 256
        s += c << (16 * (len(d) % 3))
    return s & 0xffffffffffffffff


def entries(pe):
    """Yield (code, index, name, expected_sum, file_offset_of_sum)."""
    for start, end, code in TABLES:
        a, i = start, 0
        while a < end:
            o = pe.off(a)
            name = pe.b[o:o + 0x20].split(b'\0')[0].decode('latin1').strip()
            exp, = struct.unpack_from('<Q', pe.b, o + 0x20)
            yield code, i, name, exp, o + 0x20
            a += ENTRY
            i += 1


def game_files(game):
    """Case-insensitive map name -> path (the table names are upper case)."""
    return {f.lower(): os.path.join(game, f) for f in os.listdir(game)}


def main(argv):
    args = [a for a in argv if not a.startswith('--')]
    game = args[0] if args else '.'
    exe = os.path.join(game, 'Jumpgate.exe')
    if '--exe' in argv:
        exe = argv[argv.index('--exe') + 1]
        args.remove(exe) if exe in args else None
    pe = PE(open(exe, 'rb').read())
    files = game_files(game)
    rows = []
    for code, i, name, exp, _o in entries(pe):
        p = files.get(name.lower())
        if not p:
            rows.append((code, i, name, 'missing'))
        else:
            rows.append((code, i, name, 'ok' if cks(open(p, 'rb').read()) == exp else 'MISMATCH'))
    bad = [r for r in rows if r[3] != 'ok']
    print(f'{os.path.basename(exe)}: {len(rows)} entries, {len(bad)} not ok')
    for code, i, name, st in bad:
        print(f'  D3D_ALLOC_ERROR {code}{i}  {name}  {st}')
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
