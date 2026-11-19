# Fan server: implementation docs

This folder documents **what we build**: the server, the NPC agents, our
RakNet crate, and the design choices behind them. The classic game itself
(client, protocol, crypto, classic server behaviour) is documented in
`../notes/` (index: ../notes/README.md). Keep facts about Jumpgate
Classic there and our decisions here.

- **server.md** — `jumpgate-server`: layout, request → response table,
  accounts and economy, templates, fixes.
- **agents.md** — `jumpgate-agent`: NPCs as independent client processes
  (message flow, flight model, patrol/dock AI).
- **server-design.md** — plans for building each classic mechanism (0x73,
  0x78, 0x58, 0xB3, 0x06) and the shortcuts used today.
- **raknet.md** — our `raknet` crate: where it differs from the reference and
  what it still lacks.
- **client-mods.md** — client modifications we use (re-sealed exe for modded
  assets).
- **../server/TODO.md** — the server's working checklist.

Scope and ethics are in the top-level CLAUDE.md: our own server only,
players opt in through `SERVER_IP`, no contact with jumpgate-tri.org.
