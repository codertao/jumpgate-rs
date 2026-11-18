#!/usr/bin/env python3
"""Human-readable dump of jgcap.bin (written by the Msb32 logging shim).

Like jgcap_dump.py, but shows "MSG_NAME (0xid)" per direction and decodes the
fields we understand (layouts from ../message-types.md and ../server.md).
Location ids are annotated with names from ../extracts/locations.csv.

usage: jgcap_detailed.py jgcap.bin                 # all messages
       jgcap_detailed.py jgcap.bin --type 0x08     # one type
       jgcap_dump.py-style filters: --dir C|S, --hex (append full hex),
       --no-keepalive (hide 0x8F)

Undecoded types show a short hex prefix. The login password is never printed.
"""
import argparse, csv, os, signal, struct, sys

HERE = os.path.dirname(os.path.abspath(__file__))

# ---- names (per direction; the type space is shared but meanings differ) ----

C2S = {
    0x04: 'CHAT', 0x05: 'LOGIN / LOC_REPORT', 0x07: 'TRAVEL', 0x08: 'CHANGE_SHIP', 0x09: 'COMMAND',
    0x0C: 'IMPACT', 0x0E: 'CONSUMABLES_UPDATE', 0x0F: 'CARGO_REPORT', 0x10: 'PRODBOOST',
    0x16: 'REJECT_SPAWN', 0x17: 'LOC_COMPANION', 0x1A: 'MKILL', 0x1B: 'INSURANCE_UPDATE_REQ',
    0x1D: 'HIT_QUEUE', 0x2C: 'BOT_HANDOFF', 0x2E: 'CHKBLDG', 0x32: 'MISSILE', 0x33: 'SMALL_LOC', 0x34: 'MINI_LOC',
    0x36: 'WINGMAN', 0x37: 'PACKET_55', 0x3D: 'PACKET_61', 0x3F: 'PACKET_63', 0x40: 'PACKET_64',
    0x41: 'PACKET_65', 0x43: 'COMBAT_STATS', 0x44: 'WHO_KILLED_ME', 0x4A: 'TRADE_ACK',
    0x4B: 'CONTRACT', 0x4C: 'MISSION', 0x4D: 'MODX', 0x4E: 'MENTOR', 0x4F: 'TELEPORT',
    0x59: 'HOME', 0x63: 'REQ_0x63', 0x64: 'BEACON_EVENT', 0x66: 'FITTING', 0x65: 'CHANNEL_JOIN', 0x6A: 'PLAYER_LIST_REQ', 0x71: 'BOTLOC',
    0x74: 'ASTEROID_AMOUNT', 0x75: 'ASTEROID_REQ', 0x79: 'BUILDING_2', 0x7A: 'PACKET_122',
    0x7B: 'REGISTRY_CHG', 0x83: 'LIST_CHANNELS', 0x8A: 'WEATHER_REQ', 0x8F: 'KEEPALIVE',
    0x90: 'FIND_POS_OWNER', 0x91: 'AVAILABLE_MISSIONS', 0x93: 'MODE', 0x96: 'VOLUNTEER',
    0x98: 'REFER', 0x9B: 'VOLUNTEER_OFF', 0x9C: 'EVENT_QUERY', 0x9D: 'VOUCH', 0xA0: 'LAUNCH_SPACE',
    0xA1: 'ENTER_SPACE', 0xAA: 'INVITE', 0xAB: 'INVITE_2', 0xAC: 'SHIPNAME', 0xB0: 'BOT_EVENT',
    0xBA: 'UPDPR', 0xBB: 'HOME_OFF', 0xC1: 'JOB_LIST_REQ', 0xC3: 'UTIL', 0xC5: 'COMMISSION_AWAY',
    0xC8: 'LOGOUT',
}

S2C = {
    0x03: 'DAMAGE / BOT_UPDATE', 0x04: 'CHAT', 0x05: 'SHIP_UPDATE', 0x06: 'CHAR_RECORD',
    0x08: 'LOCATION_CHANGE', 0x0A: 'LISTING', 0x0B: 'LISTING', 0x0D: 'LISTING', 0x13: 'TABLE_0x13',
    0x14: 'CONFLUX_SWARM', 0x18: 'STATUS_0x18', 0x33: 'SMALL_LOC_RELAY', 0x34: 'MINI_LOC_RELAY',
    0x42: 'HEARTBEAT_ACK', 0x45: 'CREDITS_ITEMS', 0x4B: 'CONTRACT', 0x50: 'LISTING', 0x51: 'LISTING',
    0x52: 'LISTING', 0x53: 'LISTING', 0x54: 'LISTING', 0x55: 'LISTING', 0x56: 'LISTING',
    0x58: 'TABLE_0x58', 0x61: 'BEACON_UPDATE', 0x62: 'TABLE_0x62', 0x65: 'CHANNEL_ACK', 0x70: 'BOT_SPAWN',
    0x71: 'BOT_ASSIGN', 0x77: 'MEDAL', 0x73: 'OBJECT_SLOTS', 0x76: 'MISSION_INFO?', 0x78: 'OBJECT_LIST',
    0x81: 'MOTD', 0x86: 'CLOUD_UPDATE', 0x87: 'PARTICLE_CLOUDS', 0x8C: 'STATUS_0x8C', 0x8F: 'KEEPALIVE', 0x91: 'AVAILABLE_MISSIONS',
    0x92: 'EXPERIENCE', 0xA7: 'SECTOR_STATION', 0xB3: 'OWN_SHIP_ECHO', 0xBA: 'UPDPR',
    0xC2: 'JOB_LIST', 0xFE: 'MEMORY_PATCH',
}

TRAVEL = {1: 'launch', 2: 'dock', 3: 'jump'}
# Item categories (items.md); the per-category tables are the extracts.
CATEGORY_CSV = {0: 'guns', 1: 'missiles', 3: 'cargo', 5: 'engines', 6: 'powerplants', 7: 'radars',
                8: 'ecms', 9: 'shields', 10: 'capacitors', 11: 'devices'}
LISTING_CATEGORY = {0x0A: 0, 0x0B: 1, 0x0D: 3, **{0x50 + i: 5 + i for i in range(7)}}
# C→S 0x09 buy sub-ops: category + 4 for 0/1/3, category + 5 for 5..11 (0x0a-0x0f inferred
# from the static sub list; 0x04/0x05/0x07/0x10 seen in jgcap2). 0x08 = sell from hold slot.
BUY_CATEGORY = {0x04: 0, 0x05: 1, 0x07: 3, **{0x0A + i: 5 + i for i in range(7)}}
# C→S 0x09 command → S→C listing type (capture order).
LISTING_FOR_CMD = {0x00: 0x0A, 0x01: 0x0B, 0x03: 0x0D, **{0x5A + i: 0x50 + i for i in range(7)}}
UTIL_SUBS = {0xF2: 'miltour', 0xF3: 'util-243', 0xF4: 'insurance', 0xFB: 'homewhere/oredepot?'}

# ---- location names ----

LOCS = {}
try:
    with open(os.path.join(HERE, '..', 'extracts', 'locations.csv')) as f:
        for r in csv.DictReader(f):
            if r['name']:
                LOCS[int(r['loc_id'])] = r['name']
except OSError:
    pass


ITEMS = {}  # category -> {index within category: name}
for _cat, _f in CATEGORY_CSV.items():
    try:
        with open(os.path.join(HERE, '..', 'extracts', _f + '.csv')) as f:
            _rows = list(csv.DictReader(f))
        ITEMS[_cat] = ({int(r['id']) - 222: r['name'] for r in _rows} if _cat == 3
                       else {i: r['name'] for i, r in enumerate(_rows)})
    except OSError:
        ITEMS[_cat] = {}


def item(cat, idx):
    return ITEMS.get(cat, {}).get(idx) or 'cat%d#%d' % (cat, idx)


def loc(v):
    """'118 (Quantar Depot)'; station ids < 100 dock into sector id + 100."""
    name = LOCS.get(v) or (LOCS.get(v + 100) and LOCS[v + 100] + ' station') if v < 100 else LOCS.get(v)
    return '%d (%s)' % (v, name) if name else str(v)


# ---- byte helpers ----

def u8(m, o): return m[o] if o < len(m) else None
def u16(m, o): return struct.unpack_from('<H', m, o)[0] if o + 2 <= len(m) else None
def i16(m, o): return struct.unpack_from('<h', m, o)[0] if o + 2 <= len(m) else None
def u32(m, o): return struct.unpack_from('<I', m, o)[0] if o + 4 <= len(m) else None
def f32(m, o): return struct.unpack_from('<f', m, o)[0] if o + 4 <= len(m) else None


def vec(m, o):
    if o + 12 > len(m):
        return '?'
    return '(%.1f, %.1f, %.1f)' % struct.unpack_from('<fff', m, o)


def cstr(m, o, n=None):
    end = len(m) if n is None else min(len(m), o + n)
    s = m[o:end].split(b'\0', 1)[0].split(b'\xcc', 1)[0]  # 0xCC = server's debug fill
    return s.decode('latin-1')


def wstr(m, o, n=32):
    """Space-server field: UTF-16LE, or the narrow 'none' sentinel."""
    raw = m[o:o + n]
    if raw.startswith(b'none\0'):
        return 'none'
    s = raw.decode('utf-16-le', 'replace').split('\0', 1)[0]
    return s


def short_hex(m, n=24):
    h = m[:n].hex(' ')
    return h + (' …' if len(m) > n else '')


# ---- decoders: (dir, type) → fn(m) -> str ----

def d_login(m):
    cs = cstr(m, 1, 0x15)
    return 'callsign=%r password=<hidden> version=%r' % (cs, cstr(m, 0x16))


def d_loc_report(m):
    return ('callsign=%r loc=%s ship_type=%s pid=%s clock=%.2fs pos=%s'
            % (cstr(m, 0x19, 16), loc(u16(m, 0x44)), u16(m, 0x46), u32(m, 0x5C), f32(m, 100), vec(m, 144)))


def d_c_05(m):
    if len(m) <= 64:
        return 'LOGIN: ' + d_login(m)
    if len(m) == 176:
        return 'LOC_REPORT: ' + d_loc_report(m)
    return short_hex(m)


def d_travel(m):
    act, frm, arg, pid = u8(m, 1), u16(m, 2), u32(m, 4), u32(m, 8)
    s = '%s from=%s' % (TRAVEL.get(act, 'action %s' % act), loc(frm))
    if act == 3:
        s += ' gate=%d (arg %d)' % (arg + 1, arg)
    elif arg:
        s += ' arg=%d' % arg
    return s + ' pid=%s' % pid


def d_command(m):
    cmd = u8(m, 1)
    if cmd in BUY_CATEGORY:
        return 'BUY %g x %s loc=%s pid=%s' % (f32(m, 12), item(BUY_CATEGORY[cmd], u32(m, 4)), loc(u16(m, 2)), u32(m, 8))
    if cmd == 0x08:
        return 'SELL %g from item slot %d loc=%s pid=%s' % (f32(m, 12), u32(m, 4), loc(u16(m, 2)), u32(m, 8))
    lt = LISTING_FOR_CMD.get(cmd)
    return 'cmd=0x%02x%s loc=%s pid=%s' % (cmd, ' (→ listing 0x%02x)' % lt if lt is not None else '',
                                          loc(u16(m, 2)), u32(m, 8))


def d_c_chat(m):
    cs = cstr(m, 8, 16)
    rest = m[8 + len(cs) + 1:]
    text = ''.join(chr(b) for b in rest if 32 <= b < 127).strip()
    return 'sub=%d loc=%s pid=%s from=%r text=%r' % (u8(m, 1), loc(u16(m, 2)), u32(m, 4), cs, text)


def d_channel_join(m):
    return 'loc=%s pid=%s channel=%r' % (loc(u16(m, 2)), u32(m, 4), cstr(m, 8, 16))


def d_pid4(m):
    return 'pid=%s' % u32(m, 4)


def d_small_loc(m):
    v = [i16(m, 0x12 + 2 * k) for k in range(3)]
    vel = '(%.1f, %.1f, %.1f)' % tuple(x / 10 for x in v) if None not in v else '?'
    return 'loc=%s pid=%s pos=%s vel=%s clock=%.2fs' % (loc(u16(m, 4)), u32(m, 0x18), vec(m, 0x1C), vel,
                                                        f32(m, 0x34) or 0)


def d_mini_loc(m):
    return 'loc=%s pid=%s pos=%s clock=%.2fs' % (loc(u16(m, 4)), u32(m, 0x14), vec(m, 0x18), f32(m, 0x30) or 0)


def d_botloc(m):
    return ('state=%d hull=%d%% shield=%d%% id=%s loc=%s pid@0x20=%s pid@0x24=%s'
            % (u8(m, 1), u8(m, 2), u8(m, 3), u16(m, 8), loc(u16(m, 0xA)), u32(m, 0x20), u32(m, 0x24)))


def d_asteroid(m):
    return 'sub=0x%02x asteroid=%s loc=%s pid=%s' % (u8(m, 1), u16(m, 2), loc(u16(m, 4)), u32(m, 0xC))


def d_util(m):
    sub = u8(m, 1)
    return 'sub=0x%02x (%s)' % (sub, UTIL_SUBS.get(sub, '?'))


def d_job_req(m):
    return 'sub=%d loc=%s pid=%s' % (u8(m, 1), loc(u16(m, 2)), u32(m, 4))


def d_char_record(m):
    ip = wstr(m, 0x22)
    return ('callsign=%r mode=%s loc=%s pid=%s space_ip=%r ship=%r'
            % (cstr(m, 3, 11), u8(m, 0x45), loc(u16(m, 0x50)), u32(m, 0x54), ip,
               cstr(m, 0x8F, 16) if len(m) > 0x8F else ''))


def d_location_change(m):
    sub = u8(m, 1)
    what = {6: 'launch pad'}.get(sub, 'gate %d' % sub if sub and sub <= 4 else '0x%02x' % sub)
    return 'dest=%s arrive_at=%s space_server=%r' % (loc(u16(m, 0x2C)), what, wstr(m, 0x0C))


def d_motd(m):
    return '%r / %r' % (cstr(m, 1, 0x1A9), cstr(m, 0x1AA))


def d_s_chat(m):
    sub = u8(m, 1)
    if sub == 8:  # system/news: sender @8, text @0x13
        return 'system loc=%s from=%r text=%r' % (loc(u16(m, 2)), cstr(m, 8, 11), cstr(m, 0x13))
    return d_c_chat(m)


def d_mem_patch(m):
    out, i = [], 1
    while i + 7 <= len(m) and m[i] == 1:
        addr, ln = u32(m, i + 1), u16(m, i + 5)
        data = m[i + 7:i + 7 + ln]
        txt = data.split(b'\0', 1)[0]
        printable = txt and all(32 <= b < 127 for b in txt)
        out.append('0x%08x[%d]=%s' % (addr, ln, repr(txt.decode()) if printable else data[:8].hex()))
        i += 7 + ln
    return '%d write(s): %s' % (len(out), '; '.join(out))


def d_listing(m):
    """Station market for one category: (u32 stock, u32 price) per item index."""
    cat = LISTING_CATEGORY.get(m[0], -1)
    n = len(ITEMS.get(cat, {})) if cat != 3 else 99
    out = []
    for k in range(n):
        stock, price = u32(m, 4 + 8 * k), u32(m, 8 + 8 * k)
        if stock or price:
            out.append('%s %d@%d' % (item(cat, k), stock, price))
    return '%s market, %d item(s) stock@price: %s' % (CATEGORY_CSV.get(cat, '?'), len(out), ', '.join(out))


def d_credits_items(m):
    s = 'credits=%d' % struct.unpack_from('<Q', m, 8)[0]
    if u8(m, 0x10) == 0xFD:
        return s + ' (credits only)'
    slots = []
    for k in range(48):
        o = 0x10 + 8 * k
        if o + 8 <= len(m) and m[o] != 0xFE:  # category 0 = guns
            slots.append('%d:%s x%g' % (k, item(m[o], m[o + 1]), round(f32(m, o + 4), 3)))
    return s + ' items[%s]' % ', '.join(slots)


def d_contract(m):
    if len(m) < 0x3C:
        return short_hex(m)
    job, state, n = u8(m, 8), u8(m, 9), u8(m, 0x14)
    st = 'offer %d' % (state - 0x32) if 0x33 <= state <= 0x35 else {1: 'active', 0: 'none'}.get(state, state)
    what = item(u8(m, 0x15), u8(m, 0x16)) if u8(m, 0x16) != 0xCC else '-'
    wps = ' waypoints=[%s]' % ', '.join('%s%s' % (loc(u16(m, 0x1A + 2 * i)), ' ✓' if m[0x0A + i] else '')
                                       for i in range(min(n, 10))) if n > 1 else ''
    return ('job=%d state=%s board=%d goods=%s dest=%s%s xp=%d credits=%d remaining=%g'
            % (job, st, u8(m, 0x18), what, loc(u16(m, 0x1A)), wps, u32(m, 0x30), u32(m, 0x34), f32(m, 0x38)))


def d_c_contract(m):
    op = u8(m, 9)
    o = {0x65: 'accept'}.get(op) or ('view offer %d' % op if op < 4 else 'close (%d)' % op if op >= 0x96 else op)
    return 'job=%d %s pid=%s' % (u8(m, 8), o, u32(m, 4))


def d_bot_spawn(m):
    return 'ai_type=%d (ai_types.csv row) spawn#%d' % (u8(m, 1), u16(m, 2))


def d_bot_death(m):
    return 'bot#%d killer=%s reporter=%s' % (u16(m, 2), u32(m, 4), u32(m, 8))


def d_missile(m):
    ty = u16(m, 2)
    return 'id=%d %s owner=%s pos=%s' % (u8(m, 1), 'removed' if ty == 0xFA else 'type=%s' % item(1, ty),
                                         u32(m, 0x14), vec(m, 0x1C))


def d_beacon(m):
    return 'state=%d loc=%s' % (u8(m, 1), loc(u16(m, 2)))


def d_station(m):
    cat = {2: 'public station', 4: 'player mining HQ', 8: 'special/factory'}.get(u8(m, 34), u8(m, 34))
    return 'id=%s name=%r cat=%s pos=%s' % (u32(m, 52), cstr(m, 9, 25), cat, vec(m, 36))


CLOUD_COLOUR = {1: 'red', 2: 'green', 3: 'blue', 4: 'yellow', 5: 'purple', 6: 'teal', 7: 'orange'}


def cloud(m, o):
    """One 32-byte particle-cloud record (see message-types.md 0x87)."""
    ty, kind, strength = m[o], m[o + 1], m[o + 2]
    what = 'cloud' if ty == 1 else 'type%d' % ty
    return ('%s kind=%d(%s) strength=%d r=%.1f drift=%s at=%s'
            % (what, kind, CLOUD_COLOUR.get(kind, '?'), strength, f32(m, o + 4), vec(m, o + 8), vec(m, o + 20)))


def d_clouds(m):
    recs = [cloud(m, 4 + 32 * k) for k in range(8) if 4 + 32 * k + 32 <= len(m) and m[4 + 32 * k] != 0]
    return '%d cloud(s), galaxy-map coords: %s' % (len(recs), '; '.join(recs))


def d_cloud_update(m):
    return 'slot=%d %s' % (u8(m, 1), cloud(m, 4) if len(m) >= 36 else short_hex(m))


def d_channel_ack(m):
    return 'handle=%s' % u32(m, 4)


def d_updpr(m):
    return 'sub=%d pid=%s text=%r' % (u8(m, 1), u32(m, 4), cstr(m, 8))


def d_nonzero(m):
    return '%d non-zero byte(s)' % sum(1 for b in m[1:] if b)


DECODERS = {
    ('C', 0x05): d_c_05, ('C', 0x07): d_travel, ('C', 0x09): d_command, ('C', 0x04): d_c_chat,
    ('C', 0x65): d_channel_join, ('C', 0x6A): d_pid4, ('C', 0xC1): d_job_req, ('C', 0xC8): d_pid4,
    ('C', 0x33): d_small_loc, ('C', 0x34): d_mini_loc, ('C', 0x71): d_botloc, ('C', 0x74): d_asteroid,
    ('C', 0xC3): d_util, ('C', 0x91): d_pid4, ('C', 0x4B): d_c_contract,
    ('C', 0x44): d_bot_death, ('C', 0x32): d_missile,
    ('C', 0x08): lambda m: 'ship_type=%d pid=%s' % (u8(m, 1), u32(m, 4)),
    ('C', 0x0F): lambda m: 'flag=%d pid=%s items[%s]' % (u8(m, 1), u32(m, 0x184), ', '.join(
        '%d:%s x%g' % (k, item(m[4 + 8 * k], m[5 + 8 * k]), round(f32(m, 8 + 8 * k), 3))
        for k in range(48) if 12 + 8 * k <= len(m) and m[4 + 8 * k] != 0xFE)),
    ('C', 0x64): lambda m: 'arg=%d (beacon: faction; 8 = waypoint reached; 9 = patrol event) pid=%s' % (u8(m, 1), u32(m, 4)),
    ('C', 0x43): lambda m: 'missiles fired=%d hit=%d, gun shots=%d hits=%d' % (u8(m, 1), u8(m, 2), u16(m, 4), u16(m, 6)),
    ('C', 0x66): lambda m: '%s %s item-list slot %d <-> ship slot %d' % (
        {0: 'unfit', 1: 'fit', 2: 'missile fired'}.get(u8(m, 4), 'op%d' % u8(m, 4)),
        CATEGORY_CSV.get(u8(m, 2), 'cat%d' % u8(m, 2)), u8(m, 1), u8(m, 3)),
    ('S', 0x14): lambda m: 'swarm %d now %s (from %s)' % (u32(m, 4), loc(u32(m, 8)), loc(u32(m, 12)) if u32(m, 12) else '-'),
    ('S', 0x45): d_credits_items, ('S', 0x4B): d_contract, ('S', 0x70): d_bot_spawn,
    ('S', 0x77): lambda m: 'medal id=%d' % u8(m, 1), ('S', 0x92): lambda m: 'experience=%d' % u32(m, 4),
    ('S', 0x05): d_loc_report, ('S', 0x06): d_char_record, ('S', 0x08): d_location_change,
    ('S', 0x81): d_motd, ('S', 0x04): d_s_chat, ('S', 0xFE): d_mem_patch, ('S', 0x61): d_beacon,
    ('S', 0xA7): d_station, ('S', 0x87): d_clouds, ('S', 0x86): d_cloud_update, ('S', 0x65): d_channel_ack, ('S', 0xBA): d_updpr,
    ('S', 0x33): d_small_loc, ('S', 0x34): d_mini_loc, ('S', 0x78): d_nonzero, ('S', 0x62): d_nonzero,
    ('S', 0x73): d_nonzero,
    **{('S', t): d_listing for t in (0x0A, 0x0B, 0x0D, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56)},
}

NO_BODY = {0x8F, 0x8A, 0x63, 0x42}


def describe(d, m):
    if not m:
        return '(empty)', ''
    t = m[0]
    names = C2S if d == 'C' else S2C
    name = '%s (0x%02x)' % (names.get(t, 'UNKNOWN'), t)
    if t in NO_BODY and len(m) == 1:
        return name, ''
    fn = DECODERS.get((d, t))
    try:
        body = fn(m) if fn else short_hex(m)
    except Exception as e:  # a malformed record shouldn't stop the dump
        body = 'decode error (%s): %s' % (e, short_hex(m))
    return name, body


def read(path):
    data = open(path, 'rb').read()
    i = 0
    while i + 7 <= len(data):
        d, ms, ln = struct.unpack_from('<BIH', data, i)
        i += 7
        if i + ln > len(data):
            print('warning: truncated final record', file=sys.stderr)
            break
        yield ('C' if d else 'S'), ms, data[i:i + ln]
        i += ln


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('log')
    ap.add_argument('--type', type=lambda s: int(s, 0))
    ap.add_argument('--dir', choices=['C', 'S'])
    ap.add_argument('--hex', action='store_true', help='append the full hex of each message')
    ap.add_argument('--no-keepalive', action='store_true', help='hide 0x8F keepalives')
    a = ap.parse_args()
    recs = list(read(a.log))
    if not recs:
        print('no records')
        return
    t0 = recs[0][1]
    for d, ms, m in recs:
        t = m[0] if m else None
        if a.type is not None and t != a.type:
            continue
        if a.dir and d != a.dir:
            continue
        if a.no_keepalive and t == 0x8F:
            continue
        name, body = describe(d, m)
        arrow = 'C>S' if d == 'C' else 'S>C'
        print('%9.3f %s %-28s %4dB  %s' % ((ms - t0) / 1000, arrow, name, len(m), body))
        if a.hex and not (d == 'C' and t == 0x05 and len(m) <= 64):
            for o in range(0, len(m), 32):
                print('            %03x: %s' % (o, m[o:o + 32].hex(' ')))


if __name__ == '__main__':
    if hasattr(signal, 'SIGPIPE'):  # quiet exit when piped into head
        signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    main()
