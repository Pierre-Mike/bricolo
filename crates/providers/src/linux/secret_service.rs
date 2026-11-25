//! `secrets` backend: the freedesktop Secret Service on the session bus
//! (`org.freedesktop.secrets`), which GNOME Keyring, KWallet 5.97 and later
//! and KeePassXC serve. Each account is one item in the default collection,
//! with the attributes `service` and `account` (see
//! [`crate::secrets::item_attributes`]). Secrets travel over the bus with
//! the `plain` session algorithm: the session bus is private to the user.
//!
//! The bus and the service are looked up on every call, so a keyring that
//! starts after the node still works. No session bus, or nobody serving
//! the Secret Service, answers `unavailable`. A locked keyring is unlocked
//! when the service can do so without asking; one that needs the user to
//! type a password answers `failed`, since the node has nobody to ask.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use zbus::{Connection, OwnedObjectPath, OwnedVariant, Variant};

use crate::error::CallError;
use crate::secrets::{Store, item_attributes, item_label};

const BUS_NAME: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const SESSION_IFACE: &str = "org.freedesktop.Secret.Session";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";
/// GNOME Keyring's keyring that the login password unlocks.
const LOGIN_PATH: &str = "/org/freedesktop/secrets/collection/login";
/// The object path the API uses for "none": no collection, no prompt.
const NONE: &str = "/";
/// A keyring started by D-Bus activation can take a moment; one that
/// hangs longer than this is not coming.
const TIMEOUT: Duration = Duration::from_secs(10);

/// `(session, parameters, value, content type)`: the API's `Secret` struct.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

pub struct SecretServiceStore;

impl Store for SecretServiceStore {
    fn get(&self, account: &str) -> Result<Option<String>, CallError> {
        run(async {
            let s = Session::open().await?;
            let found = s.find(account).await?;
            let result = match found.first() {
                Some(item) => {
                    let reply = s
                        .call(item.as_str(), ITEM_IFACE, "GetSecret", &(&s.path,))
                        .await?;
                    let (secret,): (Secret,) = body(&reply)?;
                    String::from_utf8(secret.2)
                        .map(Some)
                        .map_err(|_| CallError::Failed("secret is not text".into()))
                }
                None => Ok(None),
            };
            s.close().await;
            result
        })
    }

    fn set(&self, account: &str, value: &str) -> Result<(), CallError> {
        run(async {
            let s = Session::open().await?;
            let result = s.store(account, value).await;
            s.close().await;
            result
        })
    }

    fn delete(&self, account: &str) -> Result<(), CallError> {
        run(async {
            let s = Session::open().await?;
            let mut result = Ok(());
            for item in s.find(account).await? {
                let reply = s.call(item.as_str(), ITEM_IFACE, "Delete", &()).await?;
                let (prompt,): (OwnedObjectPath,) = body(&reply)?;
                if let Err(e) = no_prompt(&prompt) {
                    result = Err(e);
                    break;
                }
            }
            s.close().await;
            result
        })
    }
}

/// Runs `fut` to completion with a time limit. `Store` calls are
/// synchronous and `Secrets` makes them on Tokio's blocking pool, where
/// the runtime's handle drives the bus; with no runtime around (a plain
/// `#[test]`), a small one is made for the call.
fn run<T>(fut: impl Future<Output = Result<T, CallError>>) -> Result<T, CallError> {
    let bounded = async {
        tokio::time::timeout(TIMEOUT, fut)
            .await
            .unwrap_or(Err(CallError::Timeout))
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(bounded),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(CallError::failed)?
            .block_on(bounded),
    }
}

/// A connection to the session bus and an open `plain` session with the
/// Secret Service.
struct Session {
    bus: Connection,
    path: OwnedObjectPath,
}

impl Session {
    async fn open() -> Result<Self, CallError> {
        let bus = Connection::session().await.map_err(|e| {
            tracing::debug!("secrets: no D-Bus session bus: {e}");
            CallError::Unavailable
        })?;
        let reply = call(
            &bus,
            SERVICE_PATH,
            SERVICE_IFACE,
            "OpenSession",
            &("plain", Variant::from("")),
        )
        .await?;
        let (_, path): (OwnedVariant, OwnedObjectPath) = body(&reply)?;
        Ok(Self { bus, path })
    }

    async fn call<B>(
        &self,
        path: &str,
        iface: &str,
        method: &str,
        args: &B,
    ) -> Result<zbus::Message, CallError>
    where
        B: serde::Serialize + zbus::DynamicType,
    {
        call(&self.bus, path, iface, method, args).await
    }

    /// The unlocked items for `account`, unlocking locked ones if that
    /// needs no prompt.
    async fn find(&self, account: &str) -> Result<Vec<OwnedObjectPath>, CallError> {
        let reply = self
            .call(
                SERVICE_PATH,
                SERVICE_IFACE,
                "SearchItems",
                &(item_attributes(account),),
            )
            .await?;
        let (mut unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = body(&reply)?;
        if !locked.is_empty() {
            unlocked.extend(self.unlock(locked).await?);
        }
        Ok(unlocked)
    }

    /// Unlocks `objects`, or fails if the service would have to ask.
    async fn unlock(
        &self,
        objects: Vec<OwnedObjectPath>,
    ) -> Result<Vec<OwnedObjectPath>, CallError> {
        let reply = self
            .call(SERVICE_PATH, SERVICE_IFACE, "Unlock", &(objects,))
            .await?;
        let (unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = body(&reply)?;
        no_prompt(&prompt)?;
        Ok(unlocked)
    }

    /// Creates or replaces the item for `account` in the default collection.
    async fn store(&self, account: &str, value: &str) -> Result<(), CallError> {
        let reply = self
            .call(SERVICE_PATH, SERVICE_IFACE, "ReadAlias", &("default",))
            .await?;
        let (mut collection,): (OwnedObjectPath,) = body(&reply)?;
        if collection.as_str() == NONE {
            // GNOME Keyring leaves the alias unset until something sets
            // it, but its "login" keyring, unlocked at login, is the
            // default in all but name.
            if !self.exists(LOGIN_PATH).await? {
                return Err(CallError::Failed(
                    "the Secret Service has no default keyring".into(),
                ));
            }
            collection = OwnedObjectPath::try_from(LOGIN_PATH).map_err(CallError::failed)?;
        }
        self.unlock(vec![collection.clone()]).await?;
        let properties: HashMap<&str, Variant> = HashMap::from([
            (
                "org.freedesktop.Secret.Item.Label",
                Variant::from(item_label(account)),
            ),
            (
                "org.freedesktop.Secret.Item.Attributes",
                Variant::from(item_attributes(account)),
            ),
        ]);
        let secret = (
            &self.path,
            Vec::<u8>::new(),
            value.as_bytes().to_vec(),
            "text/plain; charset=utf8",
        );
        let reply = self
            .call(
                collection.as_str(),
                COLLECTION_IFACE,
                "CreateItem",
                &(properties, secret, true),
            )
            .await?;
        let (_, prompt): (OwnedObjectPath, OwnedObjectPath) = body(&reply)?;
        no_prompt(&prompt)
    }

    /// Whether the collection at `path` exists: it answers for its label.
    async fn exists(&self, path: &str) -> Result<bool, CallError> {
        let label = (COLLECTION_IFACE, "Label");
        match self.call(path, PROPERTIES_IFACE, "Get", &label).await {
            Ok(_) => Ok(true),
            Err(CallError::Unavailable) => Err(CallError::Unavailable),
            Err(_) => Ok(false),
        }
    }

    /// Closes the session. The service also closes it when the
    /// connection goes away, so a failure here does not matter.
    async fn close(self) {
        let _ = call(&self.bus, self.path.as_str(), SESSION_IFACE, "Close", &()).await;
    }
}

async fn call<B>(
    bus: &Connection,
    path: &str,
    iface: &str,
    method: &str,
    args: &B,
) -> Result<zbus::Message, CallError>
where
    B: serde::Serialize + zbus::DynamicType,
{
    bus.call_method(Some(BUS_NAME), path, Some(iface), method, args)
        .await
        .map_err(|e| match e {
            zbus::Error::MethodError(name, detail, _)
                if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
            {
                tracing::debug!("secrets: no Secret Service: {detail:?}");
                CallError::Unavailable
            }
            e => CallError::failed(e),
        })
}

fn body<T>(reply: &zbus::Message) -> Result<T, CallError>
where
    T: for<'d> zbus::wire::DynamicDeserialize<'d>,
{
    reply.body().deserialize().map_err(CallError::failed)
}

/// `Ok` when the service did the work without asking the user.
fn no_prompt(prompt: &OwnedObjectPath) -> Result<(), CallError> {
    if prompt.as_str() == NONE {
        Ok(())
    } else {
        Err(CallError::Failed(
            "the keyring is locked and needs a password; unlock it first".into(),
        ))
    }
}
