use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bricolo_providers::claude_code::{ClaudeCodeProvider, Session};
use bricolo_providers::clipboard::ClipboardProvider;
use bricolo_providers::disk::DiskProvider;
use bricolo_providers::notify::NotifyProvider;
use bricolo_providers::speech::SpeechProvider;
use bricolo_providers::tts::{TtsProvider, TtsStatus};
use bricolo_providers::{CallError, Caller};

type Lookup<P> = Arc<dyn Fn() -> Option<Arc<P>> + Send + Sync>;
type Attempt<'a, T> = Pin<Box<dyn Future<Output = Result<T, CallError>> + Send + 'a>>;

/// One place a call can go: this node or a peer.
pub struct Target<P: ?Sized> {
    pub name: String,
    get: Lookup<P>,
}

impl<P: ?Sized + Send + Sync + 'static> Target<P> {
    pub fn fixed(name: &str, provider: Arc<P>) -> Self {
        Self {
            name: name.to_string(),
            get: Arc::new(move || Some(provider.clone())),
        }
    }

    /// `get` returns `None` while the target is offline.
    pub fn lookup(name: &str, get: impl Fn() -> Option<Arc<P>> + Send + Sync + 'static) -> Self {
        Self {
            name: name.to_string(),
            get: Arc::new(get),
        }
    }
}

/// A provider that tries its targets in order. See the crate docs.
pub struct Fallback<P: ?Sized> {
    capability: &'static str,
    timeout: Duration,
    targets: Vec<Target<P>>,
}

impl<P: ?Sized + Send + Sync> Fallback<P> {
    pub fn new(capability: &'static str, timeout: Duration, targets: Vec<Target<P>>) -> Self {
        Self {
            capability,
            timeout,
            targets,
        }
    }

    pub fn target_names(&self) -> Vec<&str> {
        self.targets.iter().map(|t| t.name.as_str()).collect()
    }

    /// Runs `f` against each target in turn. `f` builds one attempt.
    async fn call<'a, T>(&self, f: impl Fn(Arc<P>) -> Attempt<'a, T>) -> Result<T, CallError> {
        let mut last = CallError::Unavailable;
        for target in &self.targets {
            let Some(provider) = (target.get)() else {
                tracing::debug!(capability = self.capability, target = %target.name, "offline");
                last = CallError::Unavailable;
                continue;
            };
            let result = tokio::time::timeout(self.timeout, f(provider))
                .await
                .unwrap_or(Err(CallError::Timeout));
            match result {
                Err(e) if e.try_next() => {
                    tracing::debug!(capability = self.capability, target = %target.name, "{e}, trying the next target");
                    last = e;
                }
                other => return other,
            }
        }
        Err(last)
    }
}

#[async_trait]
impl TtsProvider for Fallback<dyn TtsProvider> {
    async fn speak(
        &self,
        caller: &Caller,
        text: String,
        voice: Option<String>,
    ) -> Result<(), CallError> {
        self.call(|p| {
            let (text, voice) = (text.clone(), voice.clone());
            Box::pin(async move { p.speak(caller, text, voice).await })
        })
        .await
    }

    async fn stop(&self, caller: &Caller) -> Result<(), CallError> {
        self.call(|p| Box::pin(async move { p.stop(caller).await }))
            .await
    }

    async fn status(&self, caller: &Caller) -> Result<TtsStatus, CallError> {
        self.call(|p| Box::pin(async move { p.status(caller).await }))
            .await
    }
}

#[async_trait]
impl SpeechProvider for Fallback<dyn SpeechProvider> {
    async fn transcribe(
        &self,
        caller: &Caller,
        audio: Vec<u8>,
        format: String,
    ) -> Result<String, CallError> {
        self.call(|p| {
            let (audio, format) = (audio.clone(), format.clone());
            Box::pin(async move { p.transcribe(caller, audio, format).await })
        })
        .await
    }

    async fn transcribe_file(&self, caller: &Caller, path: String) -> Result<String, CallError> {
        self.call(|p| {
            let path = path.clone();
            Box::pin(async move { p.transcribe_file(caller, path).await })
        })
        .await
    }
}

#[async_trait]
impl DiskProvider for Fallback<dyn DiskProvider> {
    async fn read(&self, caller: &Caller, path: String) -> Result<Vec<u8>, CallError> {
        self.call(|p| {
            let path = path.clone();
            Box::pin(async move { p.read(caller, path).await })
        })
        .await
    }

    async fn write(&self, caller: &Caller, path: String, data: Vec<u8>) -> Result<(), CallError> {
        self.call(|p| {
            let (path, data) = (path.clone(), data.clone());
            Box::pin(async move { p.write(caller, path, data).await })
        })
        .await
    }

    async fn list(&self, caller: &Caller, path: String) -> Result<Vec<String>, CallError> {
        self.call(|p| {
            let path = path.clone();
            Box::pin(async move { p.list(caller, path).await })
        })
        .await
    }
}

#[async_trait]
impl ClipboardProvider for Fallback<dyn ClipboardProvider> {
    async fn get(&self, caller: &Caller) -> Result<Option<String>, CallError> {
        self.call(|p| Box::pin(async move { p.get(caller).await }))
            .await
    }

    async fn set(&self, caller: &Caller, text: String) -> Result<(), CallError> {
        self.call(|p| {
            let text = text.clone();
            Box::pin(async move { p.set(caller, text).await })
        })
        .await
    }

    async fn formats(&self, caller: &Caller) -> Result<Vec<String>, CallError> {
        self.call(|p| Box::pin(async move { p.formats(caller).await }))
            .await
    }

    async fn get_image(&self, caller: &Caller) -> Result<Option<String>, CallError> {
        self.call(|p| Box::pin(async move { p.get_image(caller).await }))
            .await
    }

    async fn set_image(&self, caller: &Caller, path: String) -> Result<(), CallError> {
        self.call(|p| {
            let path = path.clone();
            Box::pin(async move { p.set_image(caller, path).await })
        })
        .await
    }

    async fn get_files(&self, caller: &Caller) -> Result<Vec<String>, CallError> {
        self.call(|p| Box::pin(async move { p.get_files(caller).await }))
            .await
    }

    async fn set_files(&self, caller: &Caller, paths: Vec<String>) -> Result<(), CallError> {
        self.call(|p| {
            let paths = paths.clone();
            Box::pin(async move { p.set_files(caller, paths).await })
        })
        .await
    }
}

#[async_trait]
impl NotifyProvider for Fallback<dyn NotifyProvider> {
    async fn show(&self, caller: &Caller, title: String, body: String) -> Result<(), CallError> {
        self.call(|p| {
            let (title, body) = (title.clone(), body.clone());
            Box::pin(async move { p.show(caller, title, body).await })
        })
        .await
    }
}

#[async_trait]
impl ClaudeCodeProvider for Fallback<dyn ClaudeCodeProvider> {
    async fn start(
        &self,
        caller: &Caller,
        dir: String,
        prompt: String,
    ) -> Result<Session, CallError> {
        self.call(|p| {
            let (dir, prompt) = (dir.clone(), prompt.clone());
            Box::pin(async move { p.start(caller, dir, prompt).await })
        })
        .await
    }

    async fn send(
        &self,
        caller: &Caller,
        id: String,
        prompt: String,
    ) -> Result<Session, CallError> {
        self.call(|p| {
            let (id, prompt) = (id.clone(), prompt.clone());
            Box::pin(async move { p.send(caller, id, prompt).await })
        })
        .await
    }

    async fn list(&self, caller: &Caller) -> Result<Vec<Session>, CallError> {
        self.call(|p| Box::pin(async move { p.list(caller).await }))
            .await
    }

    async fn status(&self, caller: &Caller, id: String) -> Result<Session, CallError> {
        self.call(|p| {
            let id = id.clone();
            Box::pin(async move { p.status(caller, id).await })
        })
        .await
    }

    async fn stop(&self, caller: &Caller, id: String) -> Result<(), CallError> {
        self.call(|p| {
            let id = id.clone();
            Box::pin(async move { p.stop(caller, id).await })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Answers `speak` with a fixed result and records that it was asked.
    struct Fake {
        name: &'static str,
        answer: Result<(), CallError>,
        delay: Duration,
        asked: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl TtsProvider for Fake {
        async fn speak(&self, _: &Caller, _: String, _: Option<String>) -> Result<(), CallError> {
            self.asked.lock().unwrap().push(self.name);
            tokio::time::sleep(self.delay).await;
            self.answer.clone()
        }
        async fn stop(&self, _: &Caller) -> Result<(), CallError> {
            Ok(())
        }
    }

    fn fake(
        name: &'static str,
        answer: Result<(), CallError>,
        asked: &Arc<Mutex<Vec<&'static str>>>,
    ) -> Target<dyn TtsProvider> {
        let p: Arc<dyn TtsProvider> = Arc::new(Fake {
            name,
            answer,
            delay: Duration::ZERO,
            asked: asked.clone(),
        });
        Target::fixed(name, p)
    }

    async fn speak(f: &Fallback<dyn TtsProvider>) -> Result<(), CallError> {
        f.speak(&Caller::new("p"), "hi".into(), None).await
    }

    #[tokio::test]
    async fn moves_past_unavailable_and_timeout() {
        let asked = Arc::default();
        let f = Fallback::new(
            "tts",
            Duration::from_secs(1),
            vec![
                fake("desktop", Err(CallError::Unavailable), &asked),
                fake("laptop", Err(CallError::Timeout), &asked),
                fake("local", Ok(()), &asked),
            ],
        );
        assert_eq!(speak(&f).await, Ok(()));
        assert_eq!(*asked.lock().unwrap(), ["desktop", "laptop", "local"]);
    }

    #[tokio::test]
    async fn other_errors_are_final() {
        let asked = Arc::default();
        let f = Fallback::new(
            "tts",
            Duration::from_secs(1),
            vec![
                fake("desktop", Err(CallError::Denied), &asked),
                fake("local", Ok(()), &asked),
            ],
        );
        assert_eq!(speak(&f).await, Err(CallError::Denied));
        assert_eq!(*asked.lock().unwrap(), ["desktop"]);
    }

    #[tokio::test]
    async fn offline_peer_is_skipped_and_all_failing_returns_the_last_error() {
        let asked = Arc::default();
        let offline = Target::lookup("desktop", || None::<Arc<dyn TtsProvider>>);
        let f = Fallback::new(
            "tts",
            Duration::from_secs(1),
            vec![offline, fake("local", Err(CallError::Timeout), &asked)],
        );
        assert_eq!(speak(&f).await, Err(CallError::Timeout));
        let none = Fallback::new(
            "tts",
            Duration::from_secs(1),
            vec![Target::lookup("desktop", || None)],
        );
        assert_eq!(speak(&none).await, Err(CallError::Unavailable));
    }

    #[tokio::test]
    async fn slow_target_times_out() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let slow: Arc<dyn TtsProvider> = Arc::new(Fake {
            name: "desktop",
            answer: Ok(()),
            delay: Duration::from_secs(5),
            asked: asked.clone(),
        });
        let f = Fallback::new(
            "tts",
            Duration::from_millis(20),
            vec![
                Target::fixed("desktop", slow),
                fake("local", Ok(()), &asked),
            ],
        );
        assert_eq!(speak(&f).await, Ok(()));
        assert_eq!(*asked.lock().unwrap(), ["desktop", "local"]);
    }
}
