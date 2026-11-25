# 04: secrets

Read `.agents/specs/README.md` first. This track adds the capability `secrets`: small values such as API keys, kept in the macOS Keychain instead of `kv`, so they never sit in a plain file.

## Contract: `wit/secrets.wit`

```wit
/// Small secrets such as API keys, kept in the macOS Keychain. Each plugin
/// sees only its own. A name is 1 to 128 letters, digits, `.`, `_` or `-`;
/// any other name answers `failed`. Always runs on this node.
interface secrets {
  use types.{call-error};

  /// The secret, or none when it is not set.
  get: async func(name: string) -> result<option<string>, call-error>;
  set: async func(name: string, value: string) -> result<_, call-error>;
  /// Deleting a secret that is not set does nothing.
  delete: async func(name: string) -> result<_, call-error>;
}
```

## Rust: `crates/providers/src/secrets.rs`

The logic (names, scoping to the plugin) lives in `Secrets<S>`, which the tests run against an in-memory store. Only the store touches the Keychain.

```rust
#[async_trait]
pub trait SecretsProvider: Send + Sync {
    async fn get(&self, caller: &Caller, name: String) -> Result<Option<String>, CallError>;
    async fn set(&self, caller: &Caller, name: String, value: String) -> Result<(), CallError>;
    async fn delete(&self, caller: &Caller, name: String) -> Result<(), CallError>;
}

/// No secrets on this node.
pub struct NoSecrets;

/// Where `Secrets` keeps values, by account name.
pub trait Store: Send + Sync + 'static {
    fn get(&self, account: &str) -> Result<Option<String>, CallError>;
    fn set(&self, account: &str, value: &str) -> Result<(), CallError>;
    /// Removing an account that does not exist is not an error.
    fn delete(&self, account: &str) -> Result<(), CallError>;
}

/// The account for `name` in `plugin`: "<plugin>/<name>", or
/// `Failed("bad secret name")`.
pub fn account(plugin: &str, name: &str) -> Result<String, CallError>;

/// Checks names and scopes them to the calling plugin.
pub struct Secrets<S: Store> { /* the store */ }
impl<S: Store> Secrets<S> {
    pub fn new(store: S) -> Self;
}
impl<S: Store> SecretsProvider for Secrets<S> { /* ... */ }

/// Keeps values in memory. For tests.
#[derive(Default)]
pub struct MemoryStore { /* Mutex<HashMap<String, String>> */ }
```

Run blocking store calls on `tokio::task::spawn_blocking`.

## Backends

- macOS: `KeychainStore` in `crates/providers/src/macos/`. It stores generic passwords with service `bricolo`, using the `security-framework` crate's `passwords` module (`get_generic_password`, `set_generic_password`, `delete_generic_password`) as a macOS-only dependency. A missing item (`errSecItemNotFound`, -25300) is `Ok(None)` on get and `Ok(())` on delete. `Providers::local` uses `Secrets::new(KeychainStore)` only when it has a `Desktop`, so tests never touch the real Keychain (README rule 5). Otherwise it uses `NoSecrets`.
- Linux: `NoSecrets`.
- Check the license with `just deny`. `security-framework` is MIT or Apache-2.0.

## Lua

`bricolo.secrets.get(name)` returns the value or `nil`. `bricolo.secrets.set(name, value)` and `bricolo.secrets.delete(name)` return `true`, or `nil, err`.

## Tests

### Unit tests: in `crates/providers/src/secrets.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_scoped_to_the_plugin() {
        assert_eq!(account("dictation", "openai-key").unwrap(), "dictation/openai-key");
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

    #[tokio::test]
    async fn plugins_see_only_their_own_secrets() {
        let secrets = Secrets::new(MemoryStore::default());
        let (a, b) = (Caller::new("a"), Caller::new("b"));
        secrets.set(&a, "token".into(), "123".into()).await.unwrap();
        assert_eq!(secrets.get(&a, "token".into()).await.unwrap(), Some("123".into()));
        assert_eq!(secrets.get(&b, "token".into()).await.unwrap(), None);
        secrets.delete(&b, "token".into()).await.unwrap();
        assert_eq!(secrets.get(&a, "token".into()).await.unwrap(), Some("123".into()));
        secrets.delete(&a, "token".into()).await.unwrap();
        secrets.delete(&a, "token".into()).await.unwrap();
        assert_eq!(secrets.get(&a, "token".into()).await.unwrap(), None);
        assert_eq!(
            secrets.set(&a, "bad name".into(), "x".into()).await,
            Err(CallError::Failed("bad secret name".into()))
        );
    }
}
```

### macOS, ignored: in `crates/providers/src/macos/` next to `KeychainStore`

```rust
    /// Writes to the real login Keychain under a throwaway account, then
    /// removes it. Needs a GUI session; macOS may ask to allow access.
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        let store = KeychainStore;
        let account = "bricolo-test/keychain-round-trip";
        store.set(account, "v1").unwrap();
        assert_eq!(store.get(account).unwrap(), Some("v1".into()));
        store.set(account, "v2").unwrap();
        assert_eq!(store.get(account).unwrap(), Some("v2".into()));
        store.delete(account).unwrap();
        assert_eq!(store.get(account).unwrap(), None);
        store.delete(account).unwrap();
    }
```

### Lua: `crates/lua/tests/scripts.rs`

Wire `secrets: Arc::new(Secrets::new(MemoryStore::default()))` into the test `Providers`.

```rust
#[tokio::test(flavor = "multi_thread")]
async fn secrets_set_get_and_delete() {
    let mut node = Node::new();
    node.write(
        "keys.lua",
        r#"
assert(bricolo.secrets.set("token", "s3cret"))
local v = bricolo.secrets.get("token")
local none = bricolo.secrets.get("other")
assert(bricolo.secrets.delete("token"))
local gone = bricolo.secrets.get("token")
local _, err = bricolo.secrets.set("bad name", "x")
bricolo.log.info("done", v, tostring(none), tostring(gone), err)
"#,
    );
    node.start(false).await;
    node.expect_line("keys", "done s3cret nil nil failed: bad secret name")
        .await;
}
```

### Runtime: `crates/runtime/tests/plugins.rs`

Add the `caps` command `secret-get:<name>`, which logs `secret-get: {:?}` of `secrets::get(name)`.

```rust
/// `secrets` goes through the host. Tests have no main-thread loop, so the
/// node has no Keychain and the call answers unavailable.
#[tokio::test(flavor = "multi_thread")]
async fn secrets_reach_the_host_without_the_keychain() {
    let node = Node::new();
    node.install_caps(&caps_manifest(&["/bin/echo"], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    rt.fire("caps", ProviderEvent::Hotkey("secret-get:token".into()))
        .unwrap();
    assert_eq!(
        line(&rt, "caps", "secret-get:").await,
        "secret-get: Err(CallError::Unavailable)"
    );
    rt.shutdown().await;
}
```

## Done when

The README's "Done when" commands pass, including `just deny`.
