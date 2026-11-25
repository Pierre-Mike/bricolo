//! The MCP gateway: lets an AI agent use the capabilities of every node
//! in the mesh over the Model Context Protocol. [`catalog`] turns the
//! standard library WIT into tool schemas, [`policy`] narrows what the
//! agent may use, [`server`] answers MCP messages, and [`stdio`] and
//! [`http`] carry them. See docs/mcp.md.

pub mod catalog;
pub mod http;
pub mod policy;
pub mod server;
pub mod stdio;

pub use catalog::{Catalog, Method};
pub use policy::Policy;
pub use server::{Backend, CallFailure, NodeInfo, Server};
