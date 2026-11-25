//! The calling side: one stream per call.

use std::sync::Arc;
use std::time::Duration;

use bricolo_providers::{CallError, Caller};
use serde::de::DeserializeOwned;

use crate::mesh::Shared;
use crate::transport::Connection;
use crate::wire::{
    Codec, Header, Kind, MAX_CALL_FRAME, Postcard, Response, recv_value, send_value, write_frame,
};

/// Extra time the client waits past the deadline, so the server's own
/// `timeout` answer can arrive.
const GRACE: Duration = Duration::from_millis(500);

/// A peer's open connection, ready for calls.
#[derive(Clone)]
pub(crate) struct Remote {
    pub(crate) conn: Connection,
    pub(crate) peer: String,
    pub(crate) shared: Arc<Shared>,
}

impl Remote {
    /// Sends one call and waits for its result. Dropping the returned
    /// future drops the stream, which stops the call on the peer.
    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        capability: &str,
        method: &str,
        caller: &Caller,
        args: Result<Vec<u8>, CallError>,
    ) -> Result<T, CallError> {
        let args = args?;
        let deadline = self.shared.call_timeout;
        let header = Header {
            id: self.shared.next_id(),
            kind: Kind::Call,
            capability: capability.to_string(),
            method: method.to_string(),
            deadline_ms: deadline.as_millis().try_into().unwrap_or(u64::MAX),
            plugin: caller.plugin.clone(),
        };
        let attempt = async {
            let (mut send, mut recv) = self.conn.open_bi().await.map_err(|_| self.lost())?;
            send_value(&mut send, &header)
                .await
                .map_err(|_| self.lost())?;
            write_frame(&mut send, &args)
                .await
                .map_err(|_| self.lost())?;
            send.finish();
            let response: Response = recv_value(&mut recv, MAX_CALL_FRAME)
                .await
                .map_err(|_| self.lost())?;
            Postcard::decode(&response?)
        };
        let result = tokio::time::timeout(deadline + GRACE, attempt)
            .await
            .unwrap_or(Err(CallError::Timeout));
        if !matches!(result, Err(CallError::Unavailable)) {
            self.shared.touch(&self.peer);
        }
        tracing::debug!(
            peer = %self.peer,
            id = header.id,
            "{capability}.{method}: {}",
            match &result {
                Ok(_) => "ok".to_string(),
                Err(e) => e.to_string(),
            }
        );
        result
    }

    /// What a broken stream means for the caller: `denied` if the peer
    /// refused this node, otherwise `unavailable`, so the router moves on.
    pub(crate) fn lost(&self) -> CallError {
        match self.conn.close_reason() {
            Some(reason) if reason.is_refused() => CallError::Denied,
            _ => CallError::Unavailable,
        }
    }
}
