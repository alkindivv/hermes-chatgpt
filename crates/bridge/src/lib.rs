//! Browser-independent, loopback-only Responses/MCP transport.
//! Browser work and native tool execution are delegated through explicit channels.
mod prompt;
mod protocol;
mod server;
pub use protocol::*;
pub use server::*;
