//! Plugin sharing: a node fetches an installed plugin from a peer by the
//! sha256 of its `plugin.wasm`, as `plugins.lock` records it.
//!
//! It rides a media stream for the capability `plugins`, so it works over
//! every transport, and the serving node's `peers.toml` must list
//! `plugins` in the caller's `allow`. The method is the hash. The server
//! sends one frame with `Result<u64, WireError>`, the bundle's length or
//! why there is none, then the bundle in frames, then finishes.
//!
//! The mesh moves bytes only. The fetching node checks the hash of the
//! component it unpacks, and installs it with the usual approval.

use std::sync::Arc;

use async_trait::async_trait;
use bricolo_providers::{CallError, Caller};

use crate::media::{MediaStream, StreamHandler};
use crate::wire::{Codec, MAX_MEDIA_FRAME, Postcard, WireError};

/// The capability name a peer needs in its allow list.
pub const CAPABILITY: &str = "plugins";

/// The largest bundle a node sends or accepts (64 MiB, as for installs).
pub const MAX_BUNDLE: u64 = 64 << 20;

/// What a node shares: plugin bundles by the sha256 of their component.
pub trait PluginStore: Send + Sync {
    /// The `bricolo pack` bundle of the installed plugin whose `plugin.wasm`
    /// has this sha256, or `unavailable` when none does.
    fn bundle(&self, sha256: &str) -> Result<Vec<u8>, CallError>;
}

/// True for 64 lowercase hex digits.
pub fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) struct Serve(pub(crate) Arc<dyn PluginStore>);

#[async_trait]
impl StreamHandler for Serve {
    async fn serve(
        &self,
        caller: Caller,
        sha256: String,
        mut stream: MediaStream,
    ) -> Result<(), CallError> {
        let found = if is_sha256(&sha256) {
            let store = self.0.clone();
            let hash = sha256.clone();
            tokio::task::spawn_blocking(move || store.bundle(&hash))
                .await
                .unwrap_or_else(|e| Err(CallError::Failed(e.to_string())))
        } else {
            Err(CallError::Failed(format!("{sha256:?} is not a sha256")))
        };
        let head: Result<u64, WireError> = match &found {
            Ok(bytes) if bytes.len() as u64 > MAX_BUNDLE => {
                Err(WireError::Failed("the bundle is over 64 MiB".into()))
            }
            Ok(bytes) => Ok(bytes.len() as u64),
            Err(e) => Err(e.clone().into()),
        };
        let failed = head.is_err();
        stream.tx.send(&Postcard::encode(&head)?).await?;
        if !failed && let Ok(bytes) = found {
            for chunk in bytes.chunks(MAX_MEDIA_FRAME) {
                stream.tx.send(chunk).await?;
            }
            tracing::info!(peer = %caller.plugin, "shared plugin {sha256}");
        }
        stream.tx.finish();
        Ok(())
    }
}

/// Reads what [`Serve`] sends.
pub(crate) async fn receive(mut stream: MediaStream) -> Result<Vec<u8>, CallError> {
    stream.tx.finish();
    let head = stream.rx.recv().await?.ok_or(CallError::Unavailable)?;
    let head: Result<u64, WireError> = Postcard::decode(&head)?;
    let len = head?;
    if len > MAX_BUNDLE {
        return Err(CallError::Failed("the bundle is over 64 MiB".into()));
    }
    let mut bundle = Vec::with_capacity(len as usize);
    while let Some(chunk) = stream.rx.recv().await? {
        bundle.extend_from_slice(&chunk);
        if bundle.len() as u64 > len {
            return Err(CallError::Failed(
                "the peer sent more than it announced".into(),
            ));
        }
    }
    if bundle.len() as u64 != len {
        return Err(CallError::Unavailable);
    }
    Ok(bundle)
}
