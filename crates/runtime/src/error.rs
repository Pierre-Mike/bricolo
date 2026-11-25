use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{path}: {message}")]
    Parse { path: String, message: String },
    #[error("{0}")]
    Invalid(String),
    /// A download from GitHub failed.
    #[error("{0}")]
    Fetch(String),
    #[error(transparent)]
    Rules(#[from] bricolo_router::RulesError),
}

impl Error {
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }

    pub fn parse(path: &Path, message: impl std::fmt::Display) -> Self {
        Self::Parse {
            path: path.display().to_string(),
            message: message.to_string(),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
