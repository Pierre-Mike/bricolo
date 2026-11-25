//! Long-lived streams of framed byte chunks, such as a microphone feed.
//!
//! A client opens one with [`Mesh::open_stream`](crate::Mesh::open_stream).
//! The peer runs the [`StreamHandler`] registered for that capability with
//! [`Mesh::register_stream`](crate::Mesh::register_stream). Each media
//! stream is its own stream on the connection, so it never blocks calls.

use async_trait::async_trait;
use bricolo_providers::{CallError, Caller};

use crate::transport::{RecvStream, SendStream, close};
use crate::wire::{MAX_MEDIA_FRAME, ReadFailure, read_frame, write_frame};

/// Sends frames until finished or dropped.
pub struct FrameSender {
    send: SendStream,
    finished: bool,
}

impl FrameSender {
    pub(crate) fn new(send: SendStream) -> Self {
        Self {
            send,
            finished: false,
        }
    }

    /// Sends one chunk of at most 1 MiB. Waits while the peer is slow to
    /// read this stream; other streams keep moving.
    pub async fn send(&mut self, chunk: &[u8]) -> Result<(), CallError> {
        if chunk.len() > MAX_MEDIA_FRAME {
            return Err(CallError::Failed(format!(
                "frame of {} bytes is over the {MAX_MEDIA_FRAME} byte limit",
                chunk.len()
            )));
        }
        write_frame(&mut self.send, chunk)
            .await
            .map_err(|_| CallError::Unavailable)
    }

    /// Tells the peer no more frames follow.
    pub fn finish(&mut self) {
        self.finished = true;
        self.send.finish();
    }
}

impl Drop for FrameSender {
    /// Dropping without `finish` resets the stream, so the peer sees the
    /// feed cut off rather than ended.
    fn drop(&mut self) {
        if !self.finished {
            self.send.reset(close::DROPPED);
        }
    }
}

/// Receives frames until the peer finishes.
pub struct FrameReceiver {
    recv: RecvStream,
}

impl FrameReceiver {
    pub(crate) fn new(recv: RecvStream) -> Self {
        Self { recv }
    }

    /// The next chunk, or `None` once the peer has finished.
    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, CallError> {
        match read_frame(&mut self.recv, MAX_MEDIA_FRAME).await {
            Ok(chunk) => Ok(Some(chunk)),
            Err(ReadFailure::End) => Ok(None),
            Err(ReadFailure::TooLarge(n)) => Err(CallError::Failed(format!(
                "peer sent a {n} byte frame, over the limit"
            ))),
            Err(ReadFailure::Broken(_)) => Err(CallError::Unavailable),
        }
    }
}

/// Both directions of one media stream.
pub struct MediaStream {
    pub tx: FrameSender,
    pub rx: FrameReceiver,
}

/// Serves media streams for one capability on this node.
#[async_trait]
pub trait StreamHandler: Send + Sync {
    /// Runs for the life of the stream. `caller` names the peer and its
    /// plugin. Return when the peer finishes or the work is done.
    async fn serve(
        &self,
        caller: Caller,
        method: String,
        stream: MediaStream,
    ) -> Result<(), CallError>;
}
