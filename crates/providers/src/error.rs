use thiserror::Error;

/// Mirrors `call-error` in `wit/types.wit`.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CallError {
    #[error("no node offering this capability is online")]
    Unavailable,
    #[error("the deadline passed")]
    Timeout,
    #[error("permission refused")]
    Denied,
    #[error("{0}")]
    Failed(String),
}

impl CallError {
    pub fn failed(e: impl std::fmt::Display) -> Self {
        Self::Failed(e.to_string())
    }

    /// True when another target might still answer: the router moves on.
    pub fn try_next(&self) -> bool {
        matches!(self, Self::Unavailable | Self::Timeout)
    }
}
