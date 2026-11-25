//! `secrets`: small values such as API keys, kept in the macOS Keychain
//! (`macos/keychain.rs`) or, on Linux, in the freedesktop Secret Service
//! (`linux/secret_service.rs`: GNOME Keyring, KWallet, KeePassXC), or on
//! Windows in the Credential Manager (`windows/credentials.rs`). Each keeps
//! one item per account under the service [`SERVICE`]. The names and the
//! scoping to the calling plugin live here, so tests run them against
//! [`MemoryStore`].
//!
//! `secrets` never runs on a peer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;

#[async_trait]
pub trait SecretsProvider: Send + Sync {
    async fn get(&self, caller: &Caller, name: String) -> Result<Option<String>, CallError>;
    async fn set(&self, caller: &Caller, name: String, value: String) -> Result<(), CallError>;
    async fn delete(&self, caller: &Caller, name: String) -> Result<(), CallError>;
}

/// No secrets on this node.
pub struct NoSecrets;

#[async_trait]
impl SecretsProvider for NoSecrets {
    async fn get(&self, _: &Caller, _: String) -> Result<Option<String>, CallError> {
        Err(CallError::Unavailable)
    }

    async fn set(&self, _: &Caller, _: String, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn delete(&self, _: &Caller, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// Where `Secrets` keeps values, by account name.
pub trait Store: Send + Sync + 'static {
    fn get(&self, account: &str) -> Result<Option<String>, CallError>;
    fn set(&self, account: &str, value: &str) -> Result<(), CallError>;
    /// Removing an account that does not exist is not an error.
    fn delete(&self, account: &str) -> Result<(), CallError>;
}

const MAX_NAME: usize = 128;

/// The service every stored item is filed under: the Keychain's service
/// name, and the `service` attribute of a Secret Service item.
pub const SERVICE: &str = "bricolo";

/// The attributes that identify the Secret Service item for `account`.
pub fn item_attributes(account: &str) -> HashMap<&'static str, &str> {
    HashMap::from([("service", SERVICE), ("account", account)])
}

/// The label a keyring manager shows for the item for `account`.
pub fn item_label(account: &str) -> String {
    format!("{SERVICE}: {account}")
}

/// The Windows Credential Manager target name of the item for `account`:
/// "bricolo/<account>".
pub fn credential_target(account: &str) -> String {
    format!("{SERVICE}/{account}")
}

/// The account for `name` in `plugin`: "<plugin>/<name>", or
/// `Failed("bad secret name")`.
pub fn account(plugin: &str, name: &str) -> Result<String, CallError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(format!("{plugin}/{name}"))
    } else {
        Err(CallError::Failed("bad secret name".into()))
    }
}

/// Checks names and scopes them to the calling plugin.
pub struct Secrets<S: Store> {
    store: Arc<S>,
}

impl<S: Store> Secrets<S> {
    pub fn new(store: S) -> Self {
        Self {
            store: Arc::new(store),
        }
    }

    /// Runs a blocking store call off the async threads.
    async fn blocking<T, F>(&self, f: F) -> Result<T, CallError>
    where
        T: Send + 'static,
        F: FnOnce(&S) -> Result<T, CallError> + Send + 'static,
    {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(CallError::failed)?
    }
}

#[async_trait]
impl<S: Store> SecretsProvider for Secrets<S> {
    async fn get(&self, caller: &Caller, name: String) -> Result<Option<String>, CallError> {
        let account = account(&caller.plugin, &name)?;
        self.blocking(move |s| s.get(&account)).await
    }

    async fn set(&self, caller: &Caller, name: String, value: String) -> Result<(), CallError> {
        let account = account(&caller.plugin, &name)?;
        self.blocking(move |s| s.set(&account, &value)).await
    }

    async fn delete(&self, caller: &Caller, name: String) -> Result<(), CallError> {
        let account = account(&caller.plugin, &name)?;
        self.blocking(move |s| s.delete(&account)).await
    }
}

/// Keeps values in memory. For tests.
#[derive(Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<String, String>>,
}

impl Store for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, CallError> {
        Ok(self.values.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), CallError> {
        self.values
            .lock()
            .unwrap()
            .insert(account.into(), value.into());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), CallError> {
        self.values.lock().unwrap().remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_scoped_to_the_plugin() {
        assert_eq!(
            account("dictation", "openai-key").unwrap(),
            "dictation/openai-key"
        );
        assert_eq!(account("p", "A.b_c-9").unwrap(), "p/A.b_c-9");
        let long = "x".repeat(129);
        for bad in ["", "a/b", "with space", "é", long.as_str()] {
            assert_eq!(
                account("p", bad),
                Err(CallError::Failed("bad secret name".into())),
                "{bad}"
            );
        }
        assert!(account("p", &"x".repeat(128)).is_ok());
    }

    #[test]
    fn items_are_found_by_service_and_account() {
        let attrs = item_attributes("dictation/openai-key");
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs["service"], "bricolo");
        assert_eq!(attrs["account"], "dictation/openai-key");
        assert_eq!(
            item_label("dictation/openai-key"),
            "bricolo: dictation/openai-key"
        );
        assert_eq!(
            credential_target("dictation/openai-key"),
            "bricolo/dictation/openai-key"
        );
    }

    #[tokio::test]
    async fn plugins_see_only_their_own_secrets() {
        let secrets = Secrets::new(MemoryStore::default());
        let (a, b) = (Caller::new("a"), Caller::new("b"));
        secrets.set(&a, "token".into(), "123".into()).await.unwrap();
        assert_eq!(
            secrets.get(&a, "token".into()).await.unwrap(),
            Some("123".into())
        );
        assert_eq!(secrets.get(&b, "token".into()).await.unwrap(), None);
        secrets.delete(&b, "token".into()).await.unwrap();
        assert_eq!(
            secrets.get(&a, "token".into()).await.unwrap(),
            Some("123".into())
        );
        secrets.delete(&a, "token".into()).await.unwrap();
        secrets.delete(&a, "token".into()).await.unwrap();
        assert_eq!(secrets.get(&a, "token".into()).await.unwrap(), None);
        assert_eq!(
            secrets.set(&a, "bad name".into(), "x".into()).await,
            Err(CallError::Failed("bad secret name".into()))
        );
    }
}
