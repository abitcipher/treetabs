//! treetabs processes: the server that owns the store, client connections,
//! and the pieces used by the native messaging host.

pub mod bookmarks;
pub mod bridge;
pub mod client;
pub mod config;
pub mod server;

pub use client::{Connection, connect};
pub use protocol::{ClientMsg, Role, ServerMsg};
pub use treetabs_core::protocol;
