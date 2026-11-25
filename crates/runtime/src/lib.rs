//! The plugin runtime: Wasmtime embedding, plugin checks, lifecycle and
//! reload. See "Plugin lifecycle" in docs/design.md.

pub mod bundle;
pub mod engine;
pub mod error;
pub mod github;
pub mod host;
pub mod install;
pub mod lock;
pub mod manifest;
pub mod node_config;
pub mod order;
pub mod paths;
pub mod permissions;
pub mod plugin;
mod runtime;
pub mod source;
pub mod update;

pub use bricolo_providers::ProviderEvent;
pub use bricolo_providers::sensors::ActiveSensor;
pub use error::{Error, Result};
pub use paths::Paths;
pub use plugin::{PluginInfo, State};
pub use runtime::{Options, PluginStatus, Runtime, Status};
