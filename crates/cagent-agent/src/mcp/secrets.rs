use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::RuntimeError;

/// Status of one managed MCP secret without exposing its value.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpSecretStatus {
    Missing,
    Configured,
}

/// Credential backend used by `${secret:mcp/...}` references.
///
/// The complete logical reference is the key. Values use the operating-system
/// credential service when available and a verified mode-0600 file otherwise.
#[derive(Clone, Debug)]
pub struct McpSecretStore {
    fallback_path: PathBuf,
}

impl McpSecretStore {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            fallback_path: data_dir.join("credentials/mcp-secrets.json"),
        }
    }

    pub fn status(&self, reference: &str) -> Result<McpSecretStatus, RuntimeError> {
        Ok(if self.get(reference)?.is_some() {
            McpSecretStatus::Configured
        } else {
            McpSecretStatus::Missing
        })
    }

    pub fn get(&self, reference: &str) -> Result<Option<String>, RuntimeError> {
        validate_reference(reference)?;
        if let Ok(entry) = keyring::Entry::new("cagent:mcp", reference)
            && let Ok(value) = entry.get_password()
        {
            return Ok(Some(value));
        }
        Ok(self.read_fallback()?.remove(reference))
    }

    pub fn set(&self, reference: &str, value: &str) -> Result<(), RuntimeError> {
        validate_reference(reference)?;
        if value.is_empty() {
            return Err(RuntimeError::InvalidOption(
                "managed MCP secret must not be empty".into(),
            ));
        }
        if let Ok(entry) = keyring::Entry::new("cagent:mcp", reference)
            && entry.set_password(value).is_ok()
        {
            // Verify through a fresh handle. Some credential backends can
            // report a successful write and echo it through the same entry
            // while a later lookup (including the MCP restart immediately
            // after OAuth) cannot retrieve it.
            if keyring::Entry::new("cagent:mcp", reference)
                .ok()
                .and_then(|entry| entry.get_password().ok())
                .is_some_and(|stored| stored == value)
            {
                let mut fallback = self.read_fallback()?;
                if fallback.remove(reference).is_some() {
                    self.write_fallback(&fallback)?;
                }
                return Ok(());
            }
        }
        let mut fallback = self.read_fallback()?;
        fallback.insert(reference.into(), value.into());
        self.write_fallback(&fallback)
    }

    pub fn delete(&self, reference: &str) -> Result<(), RuntimeError> {
        validate_reference(reference)?;
        if let Ok(entry) = keyring::Entry::new("cagent:mcp", reference) {
            let _ = entry.delete_credential();
        }
        let mut fallback = self.read_fallback()?;
        if fallback.remove(reference).is_some() {
            self.write_fallback(&fallback)?;
        }
        Ok(())
    }

    fn read_fallback(&self) -> Result<BTreeMap<String, String>, RuntimeError> {
        match std::fs::read(&self.fallback_path) {
            Ok(mut bytes) => simd_json::serde::from_slice(&mut bytes).map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid MCP credential file: {error}"))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn write_fallback(&self, values: &BTreeMap<String, String>) -> Result<(), RuntimeError> {
        if values.is_empty() {
            match std::fs::remove_file(&self.fallback_path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
        let parent = self.fallback_path.parent().ok_or_else(|| {
            RuntimeError::InvalidOption("invalid MCP credential directory".into())
        })?;
        std::fs::create_dir_all(parent)?;
        let temporary = self.fallback_path.with_extension("json.tmp");
        let encoded = serde_json::to_vec(values)?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &self.fallback_path)?;
        Ok(())
    }
}

fn validate_reference(reference: &str) -> Result<(), RuntimeError> {
    if !reference.starts_with("mcp/")
        || reference.split('/').any(|part| {
            part.is_empty()
                || !part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        })
    {
        return Err(RuntimeError::InvalidOption(format!(
            "invalid MCP secret reference: {reference}"
        )));
    }
    Ok(())
}

pub(crate) fn referenced_secrets(value: &str) -> Result<Vec<String>, RuntimeError> {
    let mut remaining = value;
    let mut output = Vec::new();
    while let Some(start) = remaining.find("${secret:") {
        let after = &remaining[start + 9..];
        let Some(end) = after.find('}') else {
            return Err(RuntimeError::InvalidOption(
                "unterminated ${secret:mcp/...} reference".into(),
            ));
        };
        let reference = &after[..end];
        validate_reference(reference)?;
        output.push(reference.into());
        remaining = &after[end + 1..];
    }
    Ok(output)
}
