//! `secrets` backend: generic passwords in the login Keychain, service
//! `bricolo`, one item per account.

use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

use crate::error::CallError;
use crate::secrets::{SERVICE, Store};
/// `errSecItemNotFound`.
const NOT_FOUND: i32 = -25300;

pub struct KeychainStore;

impl Store for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<String>, CallError> {
        match get_generic_password(SERVICE, account) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| CallError::Failed("secret is not text".into())),
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(CallError::failed(e)),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), CallError> {
        set_generic_password(SERVICE, account, value.as_bytes()).map_err(CallError::failed)
    }

    fn delete(&self, account: &str) -> Result<(), CallError> {
        match delete_generic_password(SERVICE, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(CallError::failed(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
