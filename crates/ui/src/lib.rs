//! The parts of `bricolo-ui` that do not need a window: talking to the node
//! over its control socket, starting the node, where the hub drops down,
//! and where the panels go. `main.rs` holds the Tauri app.

pub mod client;
pub mod dropdown;
pub mod launch;
pub mod placement;
