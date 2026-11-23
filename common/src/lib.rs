//! Shared by the server and the agents: game-message layouts (`proto`), plain
//! ship data (`ship`) and the static world loaded from `notes/extracts`
//! (`world`). No sockets and no game logic.

pub mod character;
pub mod mesh;
pub mod missile;
pub mod proto;
pub mod ship;
pub mod world;
