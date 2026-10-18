//! Pure Rust contracts derived from codex-chatgpt-web v6.1.10.
//! Copyright (c) 2026 codex-chatgpt-web contributors; MIT (LICENSE.upstream).
pub mod limits;
pub mod models;
pub mod state;
pub use state::{BrowserInteractionMode, LauncherState, Surface};
#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    #[error("{0}")]
    Policy(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, ContractError>;
