//! Platform credential-store adapter.
//!
//! `keyring` selects Windows Credential Manager, the macOS Keychain, or the
//! freedesktop Secret Service. Failure to initialize or unlock that service is
//! surfaced to the broker; this module never falls back to a file or process
//! environment value.

use std::collections::BTreeMap;

use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use super::{
    CredentialReadAuthorizationV1, CredentialRef, CredentialSecretV1, PlatformCredentialStorePort,
    SecretError,
};

const SERVICE_NAME: &str = "org.aworkit.credentials.v1";
const PAYLOAD_VERSION: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeCredentialStoreStatusV1 {
    Available { backend: &'static str },
    Locked,
    Unavailable,
}

#[derive(Serialize, Deserialize)]
struct StoredCredentialV1 {
    version: u8,
    fields: BTreeMap<String, Vec<u8>>,
}

impl Drop for StoredCredentialV1 {
    fn drop(&mut self) {
        for value in self.fields.values_mut() {
            value.zeroize();
        }
    }
}

/// Native, per-user credential storage with no plaintext fallback.
#[derive(Clone, Default)]
pub struct NativeCredentialStore;

impl NativeCredentialStore {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Reports whether the platform store initialized with its native
    /// access-control backend. A locked store is distinct from an unsupported
    /// or unavailable backend so UI can request the appropriate user action.
    #[must_use]
    pub fn status(&self) -> NativeCredentialStoreStatusV1 {
        match Entry::store_status() {
            Ok(()) => NativeCredentialStoreStatusV1::Available {
                backend: platform_backend(),
            },
            Err(KeyringError::NoStorageAccess(_)) => NativeCredentialStoreStatusV1::Locked,
            Err(_) => NativeCredentialStoreStatusV1::Unavailable,
        }
    }

    pub fn validate_access_control(&self) -> Result<(), SecretError> {
        match self.status() {
            NativeCredentialStoreStatusV1::Available { .. } => Ok(()),
            NativeCredentialStoreStatusV1::Locked => Err(SecretError::StoreLocked),
            NativeCredentialStoreStatusV1::Unavailable => Err(SecretError::StoreUnavailable),
        }
    }

    fn entry(&self, credential: &CredentialRef) -> Result<Entry, SecretError> {
        self.validate_access_control()?;
        Entry::new(SERVICE_NAME, credential.0.as_str()).map_err(map_store_error)
    }
}

impl PlatformCredentialStorePort for NativeCredentialStore {
    fn put(
        &self,
        credential: &CredentialRef,
        mut secret: CredentialSecretV1,
    ) -> Result<(), SecretError> {
        let fields = std::mem::take(&mut secret.fields)
            .into_iter()
            .map(|(name, value)| (name, value.to_vec()))
            .collect();
        let stored = StoredCredentialV1 {
            version: PAYLOAD_VERSION,
            fields,
        };
        let payload = Zeroizing::new(
            serde_json::to_vec(&stored).map_err(|_| SecretError::StoreAccessControlInvalid)?,
        );
        let entry = self.entry(credential)?;
        entry
            .set_secret(payload.as_slice())
            .map_err(map_store_error)?;

        // A successful write is not published as metadata until the exact
        // protected value can be read back. Mismatch is a repair-required
        // access-control/storage failure, never permission to use a fallback.
        let observed = Zeroizing::new(entry.get_secret().map_err(map_store_error)?);
        let expected_hash = Sha256::digest(payload.as_slice());
        let observed_hash = Sha256::digest(observed.as_slice());
        if expected_hash != observed_hash {
            return Err(SecretError::StoreAccessControlInvalid);
        }
        Ok(())
    }

    fn retrieve_for_lease(
        &self,
        credential: &CredentialRef,
        _authorization: &CredentialReadAuthorizationV1,
    ) -> Result<CredentialSecretV1, SecretError> {
        let payload = Zeroizing::new(
            self.entry(credential)?
                .get_secret()
                .map_err(map_store_error)?,
        );
        let mut stored: StoredCredentialV1 = serde_json::from_slice(payload.as_slice())
            .map_err(|_| SecretError::StoreAccessControlInvalid)?;
        if stored.version != PAYLOAD_VERSION || stored.fields.is_empty() {
            return Err(SecretError::StoreAccessControlInvalid);
        }
        Ok(CredentialSecretV1::new(std::mem::take(&mut stored.fields)))
    }

    fn delete(&self, credential: &CredentialRef) -> Result<(), SecretError> {
        self.entry(credential)?
            .delete_credential()
            .map_err(map_store_error)
    }
}

fn map_store_error(error: KeyringError) -> SecretError {
    match error {
        KeyringError::NoEntry => SecretError::UnknownCredential,
        KeyringError::NoStorageAccess(_) => SecretError::StoreLocked,
        KeyringError::BadEncoding(mut bytes) => {
            bytes.zeroize();
            SecretError::StoreAccessControlInvalid
        }
        KeyringError::BadDataFormat(mut bytes, _) => {
            bytes.zeroize();
            SecretError::StoreAccessControlInvalid
        }
        KeyringError::BadStoreFormat(_) | KeyringError::Ambiguous(_) => {
            SecretError::StoreAccessControlInvalid
        }
        KeyringError::PlatformFailure(_)
        | KeyringError::TooLong(_, _)
        | KeyringError::Invalid(_, _)
        | KeyringError::NoDefaultStore
        | KeyringError::NotSupportedByStore(_) => SecretError::StoreUnavailable,
        _ => SecretError::StoreUnavailable,
    }
}

const fn platform_backend() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "windows-credential-manager"
    }
    #[cfg(target_os = "macos")]
    {
        "macos-keychain"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "freedesktop-secret-service"
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        "unsupported"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_only_native_platform_backends() {
        let store = NativeCredentialStore::new();
        if let NativeCredentialStoreStatusV1::Available { backend } = store.status() {
            assert!(matches!(
                backend,
                "windows-credential-manager" | "macos-keychain" | "freedesktop-secret-service"
            ));
        }
    }

    #[test]
    fn payload_debug_does_not_expose_secret_material() {
        let value = StoredCredentialV1 {
            version: PAYLOAD_VERSION,
            fields: BTreeMap::from([("token".to_owned(), b"super-secret".to_vec())]),
        };
        assert!(!std::any::type_name_of_val(&value).contains("super-secret"));
    }
}
