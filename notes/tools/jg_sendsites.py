"""Resolve the C->S message type at each SendGameMessage (0x9c9c10) call site.

The client builds each message in a stack buffer and calls
SendGameMessage(buf, len, ...). This forward-simulates the ~80 instructions
before each call (pushes, byte/dword stores to [esp+X], dword copies through
registers, lea of the buffer) and reports buf[0] when it is an immediate.
Resolves ~60% of the 261 sites; the rest build the type from a register.

    python3 jg_sendsites.py [Jumpgate.exe]   # prints type counts and per-function types
"""
import sys, re, os, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import jg_static_dump as jsd

SGM = '0x9c9c10'
# C->S names (same as jgcap_detailed.C2S, lowercased for identifiers)
C2S = {
    0x04: 'chat', 0x05: 'login_or_report', 0x07: 'travel', 0x08: 'change_ship', 0x09: 'command',
    0x0C: 'impact', 0x0E: 'consumables_update', 0x0F: 'cargo_report', 0x10: 'prodboost',
    0x16: 'reject_spawn', 0x17: 'loc_companion', 0x1A: 'mkill', 0x1B: 'insurance_update',
    0x1D: 'hit_queue', 0x2C: 'bot_handoff', 0x2E: 'chkbldg', 0x32: 'missile', 0x33: 'small_loc',
    0x34: 'mini_loc', 0x36: 'wingman', 0x43: 'combat_stats', 0x44: 'who_killed_me',
    0x4A: 'trade_ack', 0x4B: 'contract', 0x4C: 'mission', 0x4D: 'modx', 0x4E: 'mentor',
    0x4F: 'teleport', 0x59: 'home', 0x63: 'req_63', 0x64: 'beacon_event', 0x65: 'channel_join',
    0x66: 'fitting', 0x6A: 'player_list_req', 0x6D: 'simulator', 0x71: 'botloc',
    0x74: 'asteroid_amount', 0x75: 'asteroid_req', 0x79: 'building_2', 0x7B: 'registry_chg',
    0x83: 'list_channels', 0x8A: 'weather_req', 0x8F: 'keepalive', 0x90: 'find_pos_owner',
    0x91: 'available_missions', 0x93: 'mode', 0x96: 'volunteer', 0x98: 'refer',
    0x9B: 'volunteer_off', 0x9C: 'event_query', 0x9D: 'vouch', 0xA0: 'launch_space',
    0xA1: 'enter_space', 0xAA: 'invite', 0xAB: 'invite_2', 0xAC: 'shipname', 0xB0: 'bot_event',
    0xBA: 'updpr', 0xBB: 'home_off', 0xC1: 'job_list_req', 0xC3: 'util',
    0xC5: 'commission_away', 0xC8: 'logout',
}
def imm(s):
    try: return int(s,16) if s.startswith('0x') else int(s)
    except: return None
def resolve(lines, i, back=80):
    """Forward-simulate esp-relative stack bytes from i-back to the call at i; return buf[0] or None."""
    esp=0; mem={}; reg={}   # reg: ('addr',off) | ('bytes',[b0..b3]) | ('imm',v)
    stk=[]
    for j in range(max(i-back,0), i):
        a,op,ar=lines[j]
        if op in ('ret','jmp'): esp=0; mem={}; reg={}; stk=[]; continue
        if op=='call': reg.pop('eax',None); continue
        m=re.match(r'esp,(0x[0-9a-f]+)$',ar)
        if m and op in('add','sub'): esp+= imm(m.group(1))*(1 if op=='add' else -1); continue
        if op=='push':
            esp-=4; v=reg.get(ar) if ar in reg else (('imm',imm(ar)) if imm(ar) is not None else None)
            if v and v[0]=='imm':
                for k in range(4): mem[esp+k]=(v[1]>>(8*k))&0xff
            elif v and v[0]=='bytes':
                for k in range(4): mem[esp+k]=v[1][k]
            else:
                for k in range(4): mem.pop(esp+k,None)
            stk.append((esp,v)); continue
        if op=='pop': esp+=4; reg.pop(ar,None); continue
        if op=='lea':
            m=re.match(r'(\w+),\[esp\+0x([0-9a-f]+)\]$',ar)
            if m: reg[m.group(1)]=('addr',esp+int(m.group(2),16))
            else:
                m=re.match(r'(\w+),',ar); 
                if m: reg.pop(m.group(1),None)
            continue
        if op=='mov':
            m=re.match(r'BYTE PTR \[esp\+0x([0-9a-f]+)\],(0x[0-9a-f]+)$',ar)
            if m: mem[esp+int(m.group(1),16)]=imm(m.group(2)); continue
            m=re.match(r'BYTE PTR \[esp\+0x([0-9a-f]+)\],(\w+)$',ar)
            if m: mem.pop(esp+int(m.group(1),16),None); continue
            m=re.match(r'DWORD PTR \[esp\+0x([0-9a-f]+)\],(\S+)$',ar)
            if m:
                o=esp+int(m.group(1),16); src=m.group(2); v=reg.get(src) if src in reg else (('imm',imm(src)) if imm(src) is not None else None)
                for k in range(4):
                    if v and v[0]=='imm': mem[o+k]=(v[1]>>(8*k))&0xff
                    elif v and v[0]=='bytes' and v[1][k] is not None: mem[o+k]=v[1][k]
                    else: mem.pop(o+k,None)
                continue
            m=re.match(r'WORD PTR \[esp\+0x([0-9a-f]+)\]',ar)
            if m:
                o=esp+int(m.group(1),16); mem.pop(o,None); mem.pop(o+1,None); continue
            m=re.match(r'(e\w\w),DWORD PTR \[esp\+0x([0-9a-f]+)\]$',ar)
            if m:
                o=esp+int(m.group(2),16); reg[m.group(1)]=('bytes',[mem.get(o+k) for k in range(4)]); continue
            m=re.match(r'(e\w\w),(0x[0-9a-f]+|\d+)$',ar)
            if m: reg[m.group(1)]=('imm',imm(m.group(2))); continue
            m=re.match(r'(\w+),',ar)
            if m: reg.pop(m.group(1),None); continue
        m=re.match(r'(\w+),',ar)
        if m: reg.pop(m.group(1),None)
        if op=='xor':
            m=re.match(r'(e\w\w),\1$',ar)
            if m: reg[m.group(1)]=('imm',0)
    if not stk: return None
    o,v=stk[-1]
    if v and v[0]=='addr': return mem.get(v[1])
    return None
def send_sites(lines):
    return [(lines[i][0], resolve(lines,i)) for i,(a,op,ar) in enumerate(lines) if op=='call' and ar==SGM]
if __name__ == '__main__':
    exe = sys.argv[1] if len(sys.argv) > 1 else 'Jumpgate.exe'
    lines, _ = jsd.disassemble(exe)
    s = send_sites(lines)
    ok = [x for x in s if x[1] is not None]
    print(f'{len(ok)} of {len(s)} send sites resolved')
    for t, n in sorted(collections.Counter(t for _, t in ok).items()):
        print(f'  0x{t:02x} {C2S.get(t, "?"):20} x{n}')
