use thiserror::Error;

#[derive(Debug, Error)]
pub enum MeshError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid {file}: {message}")]
    Invalid { file: &'static str, message: String },
    #[error("cannot start the iroh endpoint: {0}")]
    Bind(String),
}

impl MeshError {
    pub(crate) fn peers(message: impl Into<String>) -> Self {
        Self::Invalid {
            file: "peers.toml",
            message: message.into(),
        }
    }
}
