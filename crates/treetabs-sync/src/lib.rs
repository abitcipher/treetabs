//! treetabs processes: the hub that owns the tree, the server around it,
//! client connections, and the pieces used by the native messaging host.
//!
//! The hub, the browser bridge and the bookmarks mirror also build for
//! wasm: the extension runs them itself when there is no companion app.

pub mod bookmarks;
pub mod bridge;
#[cfg(not(target_arch = "wasm32"))]
pub mod client;
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
pub mod hub;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;

#[cfg(not(target_arch = "wasm32"))]
pub use client::{Connection, connect};
pub use protocol::{ClientMsg, Role, ServerMsg};
pub use treetabs_core::protocol;
