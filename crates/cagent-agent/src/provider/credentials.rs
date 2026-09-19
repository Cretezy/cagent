use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ProviderError;

pub(crate) fn environment_api_key(variable: &str) -> Result<String, ProviderError> {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| crate::ProviderError {
            kind: crate::ProviderErrorKind::Authentication,
            code: "missing_api_key".into(),
            message: format!("{variable} is not set"),
            retryable: false,
            retry_after_millis: None,
            status: None,
            metadata: std::collections::BTreeMap::new(),
        })
}

/// Resolves a configured environment variable followed by documented aliases.
/// The configured variable is always tried first so deployments can rename a
/// secret without losing the conventional Google API-key fallbacks.
pub(crate) fn environment_api_key_with_fallbacks(
    configured: &str,
    fallbacks: &[&str],
) -> Result<String, ProviderError> {
    std::iter::once(configured)
        .chain(fallbacks.iter().copied())
        .find_map(|variable| environment_api_key(variable).ok())
        .ok_or_else(|| {
            ProviderError::configuration(format!(
                "no API key found in {}",
                std::iter::once(configured)
                    .chain(fallbacks.iter().copied())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ManagedCredential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_at_millis: Option<u64>,
    pub account_id: String,
    pub has_codex_entitlement: bool,
    #[serde(default)]
    pub subscription_plan: Option<String>,
}

/// A UI-managed API key. Its value is deliberately omitted from `Debug` and
/// never appears in credential reference metadata.
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ManagedApiKey {
    pub value: String,
}

impl std::fmt::Debug for ManagedApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedApiKey")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ManagedCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedCredential")
            .field("account_id", &self.account_id)
            .field("has_codex_entitlement", &self.has_codex_entitlement)
            .field("subscription_plan", &self.subscription_plan)
            .field("expires_at_millis", &self.expires_at_millis)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CredentialReference {
    pub profile: String,
    pub provider: String,
    pub account_id: String,
    pub backend: String,
    pub warning: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CredentialStore {
    profile: String,
    provider: String,
    reference_path: PathBuf,
    fallback: CredentialFile,
}

impl CredentialStore {
    pub(crate) fn new(data_dir: &Path, profile: &str, provider: &str) -> Self {
        let directory = data_dir.join("credentials");
        Self {
            profile: profile.into(),
            provider: provider.into(),
            reference_path: directory.join(format!("{provider}.reference.json")),
            fallback: CredentialFile::new(directory.join(format!("{provider}.json"))),
        }
    }

    pub(crate) fn load(&self) -> Result<Option<ManagedCredential>, ProviderError> {
        self.load_typed()
    }

    pub(crate) fn save(&self, credential: &ManagedCredential) -> Result<(), ProviderError> {
        self.save_typed(credential, &credential.account_id)
    }

    pub(crate) fn delete(&self) -> Result<(), ProviderError> {
        self.delete_typed()
    }

    pub(crate) fn api_key(data_dir: &Path, profile: &str, provider: &str) -> Self {
        let directory = data_dir.join("credentials");
        Self {
            profile: profile.into(),
            provider: provider.into(),
            reference_path: directory.join(format!("{provider}.api-key.reference.json")),
            fallback: CredentialFile::new(directory.join(format!("{provider}.api-key.json"))),
        }
    }

    pub(crate) fn load_api_key(&self) -> Result<Option<ManagedApiKey>, ProviderError> {
        self.load_typed()
    }

    pub(crate) fn save_api_key(&self, key: &ManagedApiKey) -> Result<(), ProviderError> {
        self.save_typed(key, "api-key")
    }

    pub(crate) fn delete_api_key(&self) -> Result<(), ProviderError> {
        self.delete_typed()
    }

    fn load_typed<T: for<'de> Deserialize<'de>>(&self) -> Result<Option<T>, ProviderError> {
        if let Some(reference) = self
            .load_reference()?
            .filter(|reference| reference.backend == "os")
            && let Ok(entry) = keyring::Entry::new(&self.service(), &reference.account_id)
            && let Ok(encoded) = entry.get_password()
            && let Ok(credential) = simd_json::serde::from_slice(&mut encoded.into_bytes())
        {
            return Ok(Some(credential));
        }
        self.fallback.load()
    }

    fn save_typed<T: Serialize>(
        &self,
        credential: &T,
        account_id: &str,
    ) -> Result<(), ProviderError> {
        let encoded = serde_json::to_string(credential)
            .map_err(|error| credential_error(error.to_string()))?;
        if let Ok(entry) = keyring::Entry::new(&self.service(), account_id)
            && entry.set_password(&encoded).is_ok()
        {
            let durable = keyring::Entry::new(&self.service(), account_id)
                .and_then(|fresh| fresh.get_password())
                .is_ok_and(|stored| stored == encoded);
            if durable {
                let reference = CredentialReference {
                    profile: self.profile.clone(),
                    provider: self.provider.clone(),
                    account_id: account_id.into(),
                    backend: "os".into(),
                    warning: None,
                };
                if let Err(error) = self.save_reference(&reference) {
                    let _ = entry.delete_credential();
                    return Err(error);
                }
                self.fallback.delete()?;
                return Ok(());
            }
            let _ = entry.delete_credential();
        }
        self.fallback.save(credential)?;
        self.save_reference(&CredentialReference {
            profile: self.profile.clone(),
            provider: self.provider.clone(),
            account_id: account_id.into(),
            backend: "restrictive_file".into(),
            warning: Some(
                "OS credential service unavailable; using verified restrictive-file storage".into(),
            ),
        })
    }

    fn delete_typed(&self) -> Result<(), ProviderError> {
        if let Some(reference) = self.load_reference()?
            && reference.backend == "os"
            && let Ok(entry) = keyring::Entry::new(&self.service(), &reference.account_id)
        {
            let _ = entry.delete_credential();
        }
        self.fallback.delete()?;
        match std::fs::remove_file(&self.reference_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(credential_error(error.to_string())),
        }
    }

    fn service(&self) -> String {
        format!("cagent:{}:{}", self.profile, self.provider)
    }

    fn load_reference(&self) -> Result<Option<CredentialReference>, ProviderError> {
        match std::fs::read(&self.reference_path) {
            Ok(mut bytes) => simd_json::serde::from_slice(&mut bytes)
                .map(Some)
                .map_err(|error| credential_error(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(credential_error(error.to_string())),
        }
    }

    fn save_reference(&self, reference: &CredentialReference) -> Result<(), ProviderError> {
        let parent = self
            .reference_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        create_restricted_directory(parent)?;
        let encoded = serde_json::to_vec_pretty(reference)
            .map_err(|error| credential_error(error.to_string()))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| credential_error(error.to_string()))?;
        restrict_file(temporary.as_file())?;
        temporary
            .write_all(&encoded)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| credential_error(error.to_string()))?;
        temporary
            .persist(&self.reference_path)
            .map_err(|error| credential_error(error.error.to_string()))?;
        verify_restrictions(&self.reference_path)
    }
}

/// Restrictive-file fallback for platforms where a native credential service is unavailable.
/// Secret values are intentionally never exposed through its public metadata type.
#[derive(Clone, Debug)]
pub(crate) struct CredentialFile {
    path: PathBuf,
}

impl CredentialFile {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn load<T: for<'de> Deserialize<'de>>(&self) -> Result<Option<T>, ProviderError> {
        let mut bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(credential_error(error.to_string())),
        };
        verify_restrictions(&self.path)?;
        simd_json::serde::from_slice(&mut bytes)
            .map(Some)
            .map_err(|error| credential_error(error.to_string()))
    }

    pub(crate) fn save<T: Serialize>(&self, credential: &T) -> Result<(), ProviderError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        create_restricted_directory(parent)?;
        let encoded =
            serde_json::to_vec(credential).map_err(|error| credential_error(error.to_string()))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| credential_error(error.to_string()))?;
        restrict_file(temporary.as_file())?;
        temporary
            .write_all(&encoded)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| credential_error(error.to_string()))?;
        temporary
            .persist(&self.path)
            .map_err(|error| credential_error(error.error.to_string()))?;
        verify_restrictions(&self.path)
    }

    pub(crate) fn delete(&self) -> Result<(), ProviderError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(credential_error(error.to_string())),
        }
    }
}

#[cfg(unix)]
fn create_restricted_directory(path: &Path) -> Result<(), ProviderError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(path).map_err(|error| credential_error(error.to_string()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| credential_error(error.to_string()))?;
    let mode = std::fs::metadata(path)
        .map_err(|error| credential_error(error.to_string()))?
        .permissions()
        .mode()
        & 0o777;
    if mode != 0o700 {
        return Err(credential_error(format!(
            "credential directory permissions are {mode:o}; expected 700"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_file(file: &std::fs::File) -> Result<(), ProviderError> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| credential_error(error.to_string()))
}

#[cfg(unix)]
fn verify_restrictions(path: &Path) -> Result<(), ProviderError> {
    use std::os::unix::fs::PermissionsExt as _;
    let file_mode = std::fs::metadata(path)
        .map_err(|error| credential_error(error.to_string()))?
        .permissions()
        .mode()
        & 0o777;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let directory_mode = std::fs::metadata(parent)
        .map_err(|error| credential_error(error.to_string()))?
        .permissions()
        .mode()
        & 0o777;
    if file_mode != 0o600 || directory_mode != 0o700 {
        return Err(credential_error(format!(
            "refusing credential file with permissions {file_mode:o} in directory {directory_mode:o}"
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn create_restricted_directory(_path: &Path) -> Result<(), ProviderError> {
    Err(credential_error(
        "credential-file fallback requires a verified current-user ACL on Windows",
    ))
}
#[cfg(windows)]
fn restrict_file(_file: &std::fs::File) -> Result<(), ProviderError> {
    Err(credential_error(
        "credential-file fallback is unavailable without ACL verification",
    ))
}
#[cfg(windows)]
fn verify_restrictions(_path: &Path) -> Result<(), ProviderError> {
    Err(credential_error(
        "credential-file fallback is unavailable without ACL verification",
    ))
}

fn credential_error(message: impl Into<String>) -> ProviderError {
    ProviderError::configuration(format!("credential storage: {}", message.into()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn fallback_is_restrictive_and_refuses_permission_drift() {
        let temporary = tempfile::TempDir::new().unwrap();
        let path = temporary.path().join("credentials/auth.json");
        let store = CredentialFile::new(path.clone());
        let credential = ManagedCredential {
            access_token: "secret-canary".into(),
            refresh_token: Some("refresh-canary".into()),
            id_token: None,
            expires_at_millis: None,
            account_id: "account".into(),
            has_codex_entitlement: true,
            subscription_plan: Some("pro".into()),
        };
        store.save(&credential).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            store
                .load::<ManagedCredential>()
                .unwrap()
                .unwrap()
                .access_token,
            "secret-canary"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            store
                .load::<ManagedCredential>()
                .unwrap_err()
                .to_string()
                .contains("refusing")
        );
    }
}
