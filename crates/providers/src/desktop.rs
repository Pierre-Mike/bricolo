//! The OS main thread. On macOS, hotkeys and the screen list only work on
//! the thread that runs the AppKit event loop, which must be the process's
//! first thread. [`run`] gives that thread to the event loop and runs the
//! node, Tokio included, on another one. Elsewhere it just runs the node.

/// A handle to the main-thread loop, for the providers that need it.
#[derive(Clone)]
pub struct Desktop {
    #[cfg(target_os = "macos")]
    pub(crate) main: crate::macos::main_loop::MainHandle,
}

impl std::fmt::Debug for Desktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Desktop")
    }
}

/// Runs `node` next to the platform's main-thread loop and returns its
/// result. Call it from `main`. On macOS the calling thread runs the event
/// loop until `node` returns; `node` gets `Some(Desktop)`. Elsewhere `node`
/// runs on the calling thread and gets `None`.
#[cfg(target_os = "macos")]
pub fn run<T: Send + 'static>(node: impl FnOnce(Option<Desktop>) -> T + Send + 'static) -> T {
    crate::macos::main_loop::run(move |main| node(Some(Desktop { main })))
}

#[cfg(not(target_os = "macos"))]
pub fn run<T: Send + 'static>(node: impl FnOnce(Option<Desktop>) -> T + Send + 'static) -> T {
    node(None)
}
