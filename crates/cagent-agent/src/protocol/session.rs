#![allow(clippy::needless_return, clippy::cmp_owned)] // Protocol compatibility keeps these explicit control-flow and comparison forms.
//! Session-facing protocol types and their implementations.
use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::{Local, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::sync::OnceLock;
use uuid::Uuid;

use super::errors::RuntimeError;
use super::events::API_VERSION;
use crate::config::InstructionSnapshot;
use crate::config::{ConfigSnapshot, ConfigStore};

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}

/// A human-readable, sortable identifier for a persisted conversation.
///
/// New IDs use the form `cagent-VERSION-YYMMDD.HHMM.ZZM-FF.RANDOM`, where the
/// timestamp is local time, `ZZM` encodes the local UTC offset, FF is a
/// two-digit hashed machine fingerprint, and RANDOM is six lowercase
/// hexadecimal characters. Legacy formatted IDs with eight-character random
/// suffixes and UUID IDs remain accepted so existing conversations can still
/// be reopened.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConversationId {
    bytes: [u8; 64],
    len: u8,
}

impl ConversationId {
    const PREFIX: &'static str = "cagent-";
    const LEGACY_RANDOM_LENGTH: usize = 8;
    const FINGERPRINTED_SUFFIX_LENGTH: usize = 9;

    fn from_validated(value: &str) -> Self {
        debug_assert!(value.len() <= 64);
        let mut bytes = [0; 64];
        bytes[..value.len()].copy_from_slice(value.as_bytes());
        Self {
            bytes,
            len: value.len() as u8,
        }
    }

    fn as_str(&self) -> &str {
        // Conversation IDs are created from ASCII format components or from
        // UUID strings, so this conversion is invariant-preserving.
        std::str::from_utf8(&self.bytes[..self.len as usize])
            .expect("conversation ID bytes are always valid UTF-8")
    }

    #[must_use]
    pub fn new() -> Self {
        Self::new_with_machine_fingerprint(true)
    }

    /// Creates a new ID, optionally including the hashed machine fingerprint.
    #[must_use]
    pub fn new_with_machine_fingerprint(include_machine_fingerprint: bool) -> Self {
        let now = Local::now();
        let offset_seconds = now.offset().local_minus_utc();
        let offset_minutes = offset_seconds.unsigned_abs() / 60;
        let offset_hours = offset_minutes / 60;
        let offset_remainder = offset_minutes % 60;
        let encoded_minutes = offset_remainder / 10;
        let encoded_hours = if offset_seconds >= 0 {
            offset_hours
        } else {
            100 - offset_hours
        };
        let random = Uuid::now_v7();
        let random_bytes = random.as_bytes();
        let random_suffix =
            u32::from_be_bytes([0, random_bytes[10], random_bytes[11], random_bytes[12]]);
        let fingerprint = if include_machine_fingerprint {
            machine_fingerprint()
        } else {
            0
        };

        Self::from_validated(&format!(
            "{}{}-{}.{}.{:02}{}-{:02x}.{:06x}",
            Self::PREFIX,
            env!("CARGO_PKG_VERSION"),
            now.format("%y%m%d"),
            now.format("%H%M"),
            encoded_hours,
            encoded_minutes,
            fingerprint,
            random_suffix,
        ))
    }

    fn parse_formatted(value: &str) -> Result<(), ConversationIdParseError> {
        if value.len() > 64 {
            return Err(ConversationIdParseError::TooLong);
        }
        let Some(value) = value.strip_prefix(Self::PREFIX) else {
            return Err(ConversationIdParseError::InvalidFormat);
        };
        let Some((value, random)) = value.rsplit_once('-') else {
            return Err(ConversationIdParseError::InvalidFormat);
        };
        let valid_legacy_suffix =
            random.len() == Self::LEGACY_RANDOM_LENGTH && is_lower_hex(random);
        let valid_fingerprinted_suffix = random.len() == Self::FINGERPRINTED_SUFFIX_LENGTH
            && random.as_bytes().get(2) == Some(&b'.')
            && is_lower_hex(&random[..2])
            && is_lower_hex(&random[3..]);
        if !valid_legacy_suffix && !valid_fingerprinted_suffix {
            return Err(ConversationIdParseError::InvalidRandomSuffix);
        }

        let Some((version, date_and_time_and_timezone)) = value.rsplit_once('-') else {
            return Err(ConversationIdParseError::InvalidFormat);
        };
        let Some((date, time_and_time_zone)) = date_and_time_and_timezone.split_once('.') else {
            return Err(ConversationIdParseError::InvalidFormat);
        };
        let Some((time, timezone)) = time_and_time_zone.split_once('.') else {
            return Err(ConversationIdParseError::InvalidFormat);
        };
        if version.is_empty() || version.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(ConversationIdParseError::InvalidVersion);
        }
        if date.len() != 6 || !date.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ConversationIdParseError::InvalidTimestamp);
        }
        if time.len() != 4 || !time.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ConversationIdParseError::InvalidTimestamp);
        }
        if timezone.len() != 3 || !timezone.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ConversationIdParseError::InvalidTimestamp);
        }

        NaiveDate::parse_from_str(date, "%y%m%d")
            .map_err(|_| ConversationIdParseError::InvalidTimestamp)?;
        NaiveTime::parse_from_str(time, "%H%M")
            .map_err(|_| ConversationIdParseError::InvalidTimestamp)?;
        let encoded_hours: u8 = timezone[..2]
            .parse()
            .map_err(|_| ConversationIdParseError::InvalidTimezone)?;
        let minutes: u8 = timezone[2..3]
            .parse()
            .map_err(|_| ConversationIdParseError::InvalidTimezone)?;
        if minutes >= 6 || !(encoded_hours <= 24 || (88..=99).contains(&encoded_hours)) {
            return Err(ConversationIdParseError::InvalidTimezone);
        }

        Ok(())
    }
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

static MACHINE_FINGERPRINT: OnceLock<u8> = OnceLock::new();

fn machine_fingerprint() -> u8 {
    *MACHINE_FINGERPRINT
        .get_or_init(|| machine_fingerprint_for_identifier(machine_identifier().as_deref()))
}

fn machine_fingerprint_for_identifier(identifier: Option<&str>) -> u8 {
    identifier
        .map(|identifier| Sha256::digest(identifier.as_bytes())[0])
        .unwrap_or(0)
}

fn machine_identifier() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        return ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .into_iter()
            .find_map(read_machine_identifier);
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(output) = command_output("ioreg", &["-rd1", "-c", "IOPlatformExpertDevice"]) {
            if let Some(identifier) = output
                .lines()
                .find(|line| line.contains("IOPlatformUUID"))
                .and_then(|line| line.split_once('=').map(|(_, value)| value.trim()))
                .map(|value| value.trim_matches('"').trim())
                .filter(|value| !value.is_empty())
            {
                return Some(identifier.to_owned());
            }
        }
        return command_output("hostname", &[])
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(output) = command_output(
            "reg",
            &[
                "query",
                "HKLM\\SOFTWARE\\Microsoft\\Cryptography",
                "/v",
                "MachineGuid",
            ],
        ) {
            if let Some(identifier) = output
                .lines()
                .find(|line| line.contains("MachineGuid"))
                .and_then(|line| line.split_whitespace().last())
                .filter(|value| !value.is_empty())
            {
                return Some(identifier.to_owned());
            }
        }
        return std::env::var("COMPUTERNAME")
            .ok()
            .filter(|value| !value.trim().is_empty());
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn read_machine_identifier(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn command_output(command: &str, arguments: &[&str]) -> Option<String> {
    std::process::Command::new(command)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
}

impl Default for ConversationId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ConversationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ConversationId")
            .field(&self.as_str())
            .finish()
    }
}

impl std::fmt::Display for ConversationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(formatter)
    }
}

impl std::str::FromStr for ConversationId {
    type Err = ConversationIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Ok(uuid) = Uuid::parse_str(value) {
            return Ok(Self::from_validated(&uuid.to_string()));
        }
        Self::parse_formatted(value)?;
        Ok(Self::from_validated(value))
    }
}

impl Serialize for ConversationId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ConversationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConversationIdParseError {
    TooLong,
    InvalidFormat,
    InvalidVersion,
    InvalidTimestamp,
    InvalidTimezone,
    InvalidRandomSuffix,
}

impl std::fmt::Display for ConversationIdParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::TooLong => "conversation ID is too long",
            Self::InvalidFormat => "invalid conversation ID format",
            Self::InvalidVersion => "invalid conversation ID version",
            Self::InvalidTimestamp => "invalid conversation ID timestamp",
            Self::InvalidTimezone => "invalid conversation ID timezone",
            Self::InvalidRandomSuffix => "invalid conversation ID random suffix",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ConversationIdParseError {}

#[cfg(test)]
mod conversation_id_tests {
    use super::*;

    #[test]
    fn formatted_ids_round_trip_and_serialize_as_strings() {
        let value = "cagent-0.1.1-260214.1432.950-d2.5add51";
        let id: ConversationId = value.parse().expect("valid conversation ID");

        assert_eq!(id.to_string(), value);
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{value}\""));
        assert_eq!(
            serde_json::from_str::<ConversationId>(&format!("\"{value}\"")).unwrap(),
            id
        );
    }

    #[test]
    fn generated_ids_have_the_expected_shape() {
        let id = ConversationId::new();
        let value = id.to_string();
        let body = value.strip_prefix("cagent-").unwrap();
        let (body, random) = body.rsplit_once('-').unwrap();
        let (version, date_and_time_and_timezone) = body.rsplit_once('-').unwrap();
        let (date, time_and_time_zone) = date_and_time_and_timezone.split_once('.').unwrap();
        let (time, timezone) = time_and_time_zone.split_once('.').unwrap();
        let (fingerprint, random) = random.split_once('.').unwrap();

        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        assert_eq!(date.len(), 6);
        assert!(date.bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(time.len(), 4);
        assert!(time.bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(timezone.len(), 3);
        assert!(timezone.bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(fingerprint.len(), 2);
        assert!(is_lower_hex(fingerprint));
        assert_eq!(random.len(), 6);
        assert!(is_lower_hex(random));
        assert_eq!(value.parse::<ConversationId>().unwrap(), id);
    }

    #[test]
    fn disabled_fingerprint_uses_zero_but_keeps_random_suffix_shape() {
        let value = ConversationId::new_with_machine_fingerprint(false).to_string();
        let suffix = value.rsplit_once('-').unwrap().1;
        assert!(suffix.starts_with("00."));
        assert_eq!(suffix.len(), 9);
        assert!(value.parse::<ConversationId>().is_ok());
    }

    #[test]
    fn machine_fingerprint_hashing_is_deterministic_and_missing_source_is_zero() {
        let expected = Sha256::digest(b"example-machine-id")[0];
        assert_eq!(
            machine_fingerprint_for_identifier(Some("example-machine-id")),
            expected
        );
        assert_eq!(
            machine_fingerprint_for_identifier(Some("example-machine-id")),
            machine_fingerprint_for_identifier(Some("example-machine-id"))
        );
        assert_eq!(machine_fingerprint_for_identifier(None), 0);
    }

    #[test]
    fn legacy_uuid_ids_remain_parseable() {
        let value = "0190c6a0-0000-7000-8000-000000000001";
        let id: ConversationId = value.parse().expect("valid legacy UUID");
        assert_eq!(id.to_string(), value);
    }

    #[test]
    fn formatted_ids_validate_timestamp_timezone_and_random_suffix() {
        for value in [
            "cagent-0.1.1-260214.1432.953-d2.5add51",
            "cagent-0.1.1-260214.1432.053-d2.5add51",
            "cagent-0.1.1-260214.1432.950-1ae287ab",
        ] {
            value.parse::<ConversationId>().expect("valid offset");
        }

        for value in [
            "cagent-0.1.1-260230.1432.950-1ae287ab",
            "cagent-0.1.1-260214.1432.953-D2.5add51",
            "cagent-0.1.1-260214.1432.950-d2.5add5",
            "cagent-0.1.1-260214.1432.950-d25add512",
            "cagent-0.1.1-260214.1432.950-d2.5add5!",
            "cagent-0.1.1-260214.1432.950-1AE287AB",
        ] {
            assert!(value.parse::<ConversationId>().is_err(), "{value}");
        }
    }
}

id_type!(NodeId);
id_type!(TurnId);
id_type!(CommandId);
id_type!(RequestId);
id_type!(AttemptId);
id_type!(QueuedMessageId);
id_type!(ImageAttachmentId);
id_type!(InteractionRequestId);
id_type!(BlobId);
id_type!(AgentRunId);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EventCursor(pub u64);

#[derive(Clone, Debug)]
pub struct RuntimeOptions {
    /// Directory containing `global.db`, conversation databases, and writer locks.
    pub storage_dir: PathBuf,
    /// Optional shared global-cache directory for an ephemeral conversation store.
    pub global_storage_dir: Option<PathBuf>,
    /// Whether conversation databases are persisted to disk.
    pub persist_conversations: bool,
    /// Whether conversation metadata, search rows, composer entries, and model recents are
    /// projected globally.
    pub publish_conversations: bool,
    /// Persistent credential storage directory. When unset, credentials are
    /// loaded beside the database for backwards-compatible embedded runtimes.
    pub credential_dir: Option<PathBuf>,
    /// Optional persistent Models.dev metadata cache path. This is separate
    /// from the database so ephemeral sessions can still use model metadata.
    pub models_dev_cache_path: Option<PathBuf>,
    pub permissions_path: Option<PathBuf>,
    pub command_channel_capacity: usize,
    pub event_channel_capacity: usize,
    pub config: ConfigStore,
    pub instructions: InstructionSnapshot,
    pub instruction_paths: Option<(PathBuf, PathBuf)>,
    /// Enables trusted-project configuration for this runtime without persisting trust.
    pub temporary_workspace_trust: bool,
    /// Conversation requested by a frontend before startup cleanup can acquire its writer lock.
    pub cleanup_protected_conversation: Option<ConversationId>,
    /// Optional root for conversation scratchpads. Defaults to `CAGENT_TMPDIR`,
    /// then the operating system temporary directory.
    pub temporary_dir: Option<PathBuf>,
}

impl RuntimeOptions {
    #[must_use]
    pub fn new(storage_dir: PathBuf) -> Self {
        #[cfg(test)]
        let config = ConfigStore::in_memory(ConfigSnapshot::test_default());
        #[cfg(not(test))]
        let config = ConfigStore::in_memory(ConfigSnapshot::default());
        Self {
            storage_dir,
            global_storage_dir: None,
            persist_conversations: true,
            publish_conversations: true,
            credential_dir: None,
            models_dev_cache_path: None,
            permissions_path: None,
            command_channel_capacity: 64,
            event_channel_capacity: 256,
            config,
            instructions: InstructionSnapshot::default(),
            instruction_paths: None,
            temporary_workspace_trust: false,
            cleanup_protected_conversation: None,
            temporary_dir: None,
        }
    }

    #[must_use]
    pub fn with_temporary_dir(mut self, temporary_dir: PathBuf) -> Self {
        self.temporary_dir = Some(temporary_dir);
        self
    }

    /// Uses global catalog/composer caches from `path` while keeping the
    /// conversation database under this runtime's own storage directory.
    #[must_use]
    pub fn with_global_storage_dir(mut self, path: PathBuf) -> Self {
        self.global_storage_dir = Some(path);
        self
    }

    /// Uses an in-memory conversation database and does not publish it globally.
    #[must_use]
    pub fn without_session_persistence(mut self) -> Self {
        self.persist_conversations = false;
        self.publish_conversations = false;
        self
    }

    /// Persists conversations locally without projecting them into global indexes.
    #[must_use]
    pub fn without_conversation_indexing(mut self) -> Self {
        self.publish_conversations = false;
        self
    }

    #[must_use]
    pub fn with_config(mut self, config: impl Into<ConfigStore>) -> Self {
        self.config = config.into();
        #[cfg(test)]
        self.config.disable_implicit_title_generation_for_tests();
        self
    }

    #[must_use]
    pub fn with_models_dev_cache_path(mut self, path: PathBuf) -> Self {
        self.models_dev_cache_path = Some(path);
        self
    }

    #[must_use]
    pub fn with_credential_dir(mut self, path: PathBuf) -> Self {
        self.credential_dir = Some(path);
        self
    }

    /// Excludes a startup resume target from cleanup until its writer lock is acquired.
    #[must_use]
    pub fn with_cleanup_protected_conversation(mut self, id: ConversationId) -> Self {
        self.cleanup_protected_conversation = Some(id);
        self
    }

    #[must_use]
    pub fn with_instructions(mut self, instructions: InstructionSnapshot) -> Self {
        self.instructions = instructions;
        self
    }

    #[must_use]
    pub fn with_instruction_paths(mut self, global_root: PathBuf, workspace: PathBuf) -> Self {
        self.instruction_paths = Some((global_root, workspace));
        self
    }

    #[must_use]
    pub fn with_permissions_file(mut self, path: PathBuf) -> Self {
        self.permissions_path = Some(path);
        self
    }

    /// Trust project configuration for this runtime only.
    #[must_use]
    pub fn with_temporary_workspace_trust(mut self) -> Self {
        self.temporary_workspace_trust = true;
        self
    }
}

#[derive(Clone, Debug)]
pub struct NewSession {
    pub workspace: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationSummary {
    pub id: ConversationId,
    /// Immutable launch directory used for history and trust scoping.
    pub workspace: PathBuf,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub active_node_id: NodeId,
    pub status: String,
    pub agent: String,
    pub mode: String,
    pub model: Option<String>,
    pub message_count: u64,
    pub archived: bool,
    pub favourite: bool,
    /// Newest non-empty user or assistant content on the active branch.
    pub preview: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceVcs {
    Git,
    Jujutsu,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorktreeMetadata {
    pub vcs: WorkspaceVcs,
    pub name: String,
    pub branch_or_workspace: String,
    pub path: PathBuf,
}

/// Frontend-neutral filtering for durable conversation discovery.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationQuery {
    pub workspace: Option<PathBuf>,
    pub search: Option<String>,
    /// Include archived conversations. They are hidden by default.
    pub include_archived: bool,
}

impl ConversationSummary {
    /// Whether this conversation has not received a meaningful title yet.
    #[must_use]
    pub fn is_untitled(&self) -> bool {
        self.title.trim().is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionStats {
    pub id: ConversationId,
    #[serde(default)]
    pub title: Option<String>,
    pub message_count: u64,
    pub total_tokens: u64,
    pub total_cost: Option<String>,
    pub currency: Option<String>,
    pub usage: SessionUsage,
    pub cost_source: Option<CostSource>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    ProviderReported,
    ProviderCatalog,
    ModelsDev,
    Mixed,
}

/// Aggregated usage prepared by the agent layer for frontends.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionUsage {
    pub input_tokens: Option<u64>,
    pub non_cached_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost: Option<crate::ModelCost>,
    pub cost_complete: bool,
    pub cost_source: Option<CostSource>,
    #[serde(default)]
    calls: u64,
}

impl SessionUsage {
    #[must_use]
    pub fn calls(&self) -> u64 {
        self.calls
    }

    pub fn add(&mut self, usage: &crate::ModelUsage) {
        let first = self.calls == 0;
        self.calls = self.calls.saturating_add(1);
        if first {
            self.cost_complete = true;
        }
        add_optional(&mut self.input_tokens, usage.input_tokens);
        add_optional(
            &mut self.non_cached_input_tokens,
            usage.non_cached_input_tokens,
        );
        add_optional(
            &mut self.cache_read_input_tokens,
            usage.cache_read_input_tokens,
        );
        add_optional(
            &mut self.cache_write_input_tokens,
            usage.cache_write_input_tokens,
        );
        add_optional(&mut self.output_tokens, usage.output_tokens);
        add_optional(&mut self.reasoning_tokens, usage.reasoning_tokens);
        add_optional(&mut self.total_tokens, usage.total_tokens);

        let Some(cost) = usage.cost.as_ref() else {
            self.cost_complete = false;
            return;
        };
        let Some(total_cost) = cost.total_cost.as_deref() else {
            self.cost_complete = false;
            return;
        };
        if self.cost.is_none() {
            self.cost = Some(cost.clone());
        } else if let Some(existing) = self.cost.as_mut() {
            if existing.currency != cost.currency {
                self.cost_complete = false;
                return;
            }
            existing.input_cost =
                sum_cost(existing.input_cost.as_deref(), cost.input_cost.as_deref());
            existing.cache_read_cost = sum_cost(
                existing.cache_read_cost.as_deref(),
                cost.cache_read_cost.as_deref(),
            );
            existing.cache_write_cost = sum_cost(
                existing.cache_write_cost.as_deref(),
                cost.cache_write_cost.as_deref(),
            );
            existing.output_cost =
                sum_cost(existing.output_cost.as_deref(), cost.output_cost.as_deref());
            existing.reasoning_cost = sum_cost(
                existing.reasoning_cost.as_deref(),
                cost.reasoning_cost.as_deref(),
            );
            existing.total_cost = sum_cost(existing.total_cost.as_deref(), Some(total_cost));
            if existing.pricing_source != cost.pricing_source {
                existing.pricing_source = "mixed".into();
            }
            if existing.pricing_version != cost.pricing_version {
                existing.pricing_version = "mixed".into();
            }
        }
        let source = cost_source(cost);
        self.cost_source = match (self.cost_source, source) {
            (None, source) => Some(source),
            (Some(existing), source) if existing == source => Some(existing),
            (Some(_), _) => Some(CostSource::Mixed),
        };
    }

    #[must_use]
    pub fn cache_rate_percent(&self) -> Option<u64> {
        crate::provider::prompt_cache::cache_rate_percent(
            self.input_tokens,
            self.cache_read_input_tokens,
            self.cache_write_input_tokens,
        )
    }

    /// Returns the user-facing token total: uncached input plus output.
    ///
    /// Cache reads remain available separately because they occupy context but
    /// are not newly processed input. When a provider omits the normalized
    /// uncached value, fall back to its inclusive input count.
    #[must_use]
    pub fn display_total_tokens(&self) -> Option<u64> {
        let input = self
            .non_cached_input_tokens
            .or_else(|| {
                self.input_tokens
                    .zip(self.cache_read_input_tokens)
                    .map(|(input, cached)| input.saturating_sub(cached))
            })
            .or(self.input_tokens);
        match (input, self.output_tokens) {
            (None, None) => self.total_tokens,
            (input, output) => Some(
                input
                    .unwrap_or_default()
                    .saturating_add(output.unwrap_or_default()),
            ),
        }
    }

    #[must_use]
    pub fn total_cost(&self) -> Option<&crate::ModelCost> {
        (self.cost_complete && self.calls > 0)
            .then_some(self.cost.as_ref())
            .flatten()
    }
}

/// Usage totals displayed by global usage frontends.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UsageOverview {
    pub current_conversation: SessionUsage,
    pub rolling_24_hours: SessionUsage,
    pub rolling_7_days: SessionUsage,
    pub rolling_30_days: SessionUsage,
    pub total: SessionUsage,
}

/// One all-time usage breakdown row.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UsageBreakdown {
    pub label: String,
    pub usage: SessionUsage,
}

fn add_optional(target: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *target = Some(target.unwrap_or_default().saturating_add(value));
    }
}

fn sum_cost(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => {
            crate::runtime::pricing::decimal_sum([left, right].into_iter())
        }
        (Some(value), None) | (None, Some(value)) => Some(value.into()),
        (None, None) => None,
    }
}

fn cost_source(cost: &crate::ModelCost) -> CostSource {
    match cost.pricing_source.as_str() {
        "provider_reported" => CostSource::ProviderReported,
        "models_dev" => CostSource::ModelsDev,
        _ => CostSource::ProviderCatalog,
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HistoryNode {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub turn_id: Option<TurnId>,
    pub owner_id: Option<NodeId>,
    pub request_index: Option<u64>,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub role: Option<String>,
    pub summary: Option<String>,
    pub content: Value,
    /// The original composer submission when this node came from a slash
    /// command that also submitted a user message, such as `/plan <message>`.
    /// The model-facing content remains in `content` without the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_text: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub active: bool,
}

impl HistoryNode {
    /// Converts a hydrated durable node into the incremental event shape used
    /// by transcript replay. Persistence-only fields intentionally remain on
    /// the node. Title metadata replays the same branch-anchored event as a
    /// live title update rather than becoming a transcript notice.
    #[must_use]
    pub fn into_appended_event(
        self,
        conversation_id: ConversationId,
        cursor: EventCursor,
    ) -> DurableEvent {
        if self.kind == NodeKind::System
            && self.content["system_type"] == "title_change"
            && let Ok(SystemNodePayload::TitleChange { title, source }) =
                serde_json::from_value(self.content.clone())
        {
            return DurableEvent {
                version: API_VERSION,
                cursor,
                conversation_id,
                kind: DurableEventKind::ConversationTitleChanged {
                    title,
                    source,
                    node_id: self.parent_id,
                },
            };
        }
        DurableEvent {
            version: API_VERSION,
            cursor,
            conversation_id,
            kind: DurableEventKind::NodeAppended {
                node_id: self.id,
                parent_id: self.parent_id,
                turn_id: self.turn_id,
                owner_id: self.owner_id,
                request_index: self.request_index,
                node_kind: self.kind,
                status: self.status.to_string(),
                content: self.content,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Running,
    Streaming,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
}

impl std::fmt::Display for NodeStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Streaming => "streaming",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        })
    }
}

impl std::str::FromStr for NodeStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "streaming" => Ok(Self::Streaming),
            "completed" => Ok(Self::Completed),
            "interrupted" => Ok(Self::Interrupted),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("unknown node status: {other}")),
        }
    }
}

impl From<&str> for NodeStatus {
    fn from(value: &str) -> Self {
        value.parse().expect("valid node status")
    }
}

impl PartialEq<str> for NodeStatus {
    fn eq(&self, other: &str) -> bool {
        self.to_string() == other
    }
}

impl PartialEq<&str> for NodeStatus {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AttachmentSpec {
    pub path: PathBuf,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
}

/// Durable metadata for a normalized clipboard image. Image bytes live only
/// in the conversation blob table and are loaded explicitly by ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImageAttachment {
    pub id: ImageAttachmentId,
    pub number: u64,
    pub sha256: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: u64,
    pub blob_id: crate::BlobId,
}

/// One semantic occurrence of an atomic image chip in `UserDraft::text`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImageChipRange {
    pub image_id: ImageAttachmentId,
    pub start: usize,
    pub end: usize,
}

/// Agent-owned representation of a user submission across frontends.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserDraft {
    pub text: String,
    #[serde(default)]
    pub attachment_specs: Vec<AttachmentSpec>,
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
    #[serde(default)]
    pub image_chips: Vec<ImageChipRange>,
}

/// One recallable composer submission.
///
/// Attachment specs preserve the composer representation without retaining a
/// frontend-specific chip model.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ComposerHistoryEntry {
    #[serde(default)]
    pub kind: ComposerInputKind,
    pub text: String,
    pub attachment_specs: Vec<AttachmentSpec>,
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
    #[serde(default)]
    pub image_chips: Vec<ImageChipRange>,
}

/// How a recalled composer entry should be interpreted by a frontend.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposerInputKind {
    #[default]
    Prompt,
    Bash,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapturedAttachment {
    pub path: PathBuf,
    pub start_line: u64,
    pub end_line: u64,
    pub sha256: String,
    pub size_bytes: u64,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueTarget {
    NextBoundary,
    EndOfTurn,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedItemKind {
    #[default]
    Prompt,
    /// A direct mode command with a prompt, such as `/plan review this`.
    /// The mode is applied only when this row reaches its queue boundary.
    ModePrompt,
    Compact,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl AgentRunStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AgentRun {
    pub id: AgentRunId,
    pub conversation_id: ConversationId,
    pub parent_turn_id: TurnId,
    pub sequence: u64,
    pub profile: String,
    pub model: crate::ModelRef,
    /// Exact reasoning effort resolved when this delegated run was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub task: String,
    pub status: AgentRunStatus,
    pub result: Option<String>,
    pub error: Option<String>,
    pub usage: Option<crate::ModelUsage>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    /// Ordered, durable transcript entries produced by the delegated run.
    /// This is distinct from the concise completion result delivered back to
    /// the parent agent.
    #[serde(default)]
    pub timeline: Vec<AgentRunTimelineEntry>,
    /// Legacy completed tool-only projection retained for callers that need a
    /// compact run summary. New presentation code should use `timeline`.
    #[serde(default)]
    pub activity: Vec<AgentRunActivity>,
}

/// One durable item in a delegated agent's chronological transcript.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // Durable timeline entries preserve their direct serialized shape.
pub enum AgentRunTimelineEntry {
    Assistant {
        sequence: u64,
        text: String,
        created_at: String,
    },
    Tool {
        #[serde(flatten)]
        activity: AgentRunActivity,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AgentRunActivity {
    pub sequence: u64,
    pub tool: String,
    pub arguments: Value,
    pub output: Value,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_audit: Option<crate::PermissionAudit>,
    pub created_at: String,
}

impl QueueTarget {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NextBoundary => "next_boundary",
            Self::EndOfTurn => "end_of_turn",
        }
    }
}

impl std::str::FromStr for QueueTarget {
    type Err = RuntimeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "next_boundary" => Ok(Self::NextBoundary),
            "end_of_turn" => Ok(Self::EndOfTurn),
            value => Err(RuntimeError::InvalidOption(format!(
                "invalid stored queue target: {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QueuedMessage {
    pub id: QueuedMessageId,
    pub position: u64,
    pub target: QueueTarget,
    #[serde(default)]
    pub kind: QueuedItemKind,
    /// The configured mode to apply at dispatch for a queued mode command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The original slash command shown in the queue and restored for editing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_text: Option<String>,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<AttachmentSpec>,
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
    #[serde(default)]
    pub image_chips: Vec<ImageChipRange>,
    /// True only while this durable row is held for runtime startup resources.
    #[serde(default)]
    pub blocked_by_startup: bool,
    /// Requests one turn-local instruction requiring delegated work.
    #[serde(default)]
    pub require_subagent: bool,
}

/// Readiness of resources that are hydrated after the first session frame.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartupResourceStatus {
    pub instructions: StartupInstructionsStatus,
    pub models: StartupModelsStatus,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupInstructionsStatus {
    #[default]
    Loading,
    Ready,
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupModelsStatus {
    #[default]
    Loading,
    Ready,
}

impl StartupResourceStatus {
    #[must_use]
    pub fn ready_for_dispatch(&self) -> bool {
        matches!(self.instructions, StartupInstructionsStatus::Ready)
            && matches!(self.models, StartupModelsStatus::Ready)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionCommand {
    /// Stable identity assigned by the caller before the command is submitted.
    /// View updates caused by this command carry this value as their origin.
    pub id: CommandId,
    pub version: u16,
    pub action: SessionAction,
}

impl SessionCommand {
    #[must_use]
    pub fn new(action: SessionAction) -> Self {
        Self {
            id: CommandId::new(),
            version: API_VERSION,
            action,
        }
    }

    #[must_use]
    pub fn submit_input(text: impl Into<String>) -> Self {
        Self::new(SessionAction::SubmitInput { text: text.into() })
    }

    #[must_use]
    pub fn submit_structured_input(text: impl Into<String>, schema: Value) -> Self {
        Self::new(SessionAction::SubmitStructuredInput {
            text: text.into(),
            schema,
        })
    }

    #[must_use]
    pub fn submit_exec_input(
        text: impl Into<String>,
        schema: Option<Value>,
        tool_policy: ToolPolicy,
    ) -> Self {
        Self::new(SessionAction::SubmitExecInput {
            text: text.into(),
            schema,
            tool_policy,
        })
    }
}

/// Per-turn narrowing of the tools exposed to a model request.
///
/// A deny entry always wins when a tool is present in both lists. An omitted
/// allow list means all otherwise available tools are eligible.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolPolicy {
    pub allow: Option<BTreeSet<String>>,
    #[serde(default)]
    pub deny: BTreeSet<String>,
}

impl ToolPolicy {
    #[must_use]
    pub fn allows(&self, tool: &str) -> bool {
        self.allow.as_ref().is_none_or(|allow| allow.contains(tool)) && !self.deny.contains(tool)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allow.as_ref().is_none_or(BTreeSet::is_empty) && self.deny.is_empty()
    }
}

/// Runtime-owned state of the active turn. `started_at` is a Unix-millisecond timestamp;
/// frontends format elapsed time but never start their own timer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnState {
    Idle,
    Working {
        started_at: String,
        turn_id: TurnId,
    },
    /// A provider request is in flight. This is an observable request phase,
    /// not a claim that the provider exposes its hidden chain of thought.
    Thinking {
        started_at: String,
        turn_id: TurnId,
    },
    Waiting {
        started_at: String,
        turn_id: TurnId,
    },
    Cancelling {
        started_at: String,
        turn_id: TurnId,
    },
    Compacting {
        started_at: String,
        turn_id: TurnId,
    },
}

/// Runtime-internal accumulation state used while durable events are reduced
/// into the public transcript block model.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TranscriptProjection {
    pub(crate) entries: Vec<TranscriptEntry>,
    pub(crate) live_assistant: Option<LiveMarkdown>,
    pub(crate) live_plan: Option<LiveMarkdown>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LiveMarkdown {
    pub(crate) source: String,
    pub(crate) document: crate::MarkdownDocument,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TranscriptEntry {
    User {
        node_id: NodeId,
        label: Option<String>,
        text: String,
        images: Vec<ImageAttachment>,
        image_chips: Vec<ImageChipRange>,
    },
    Assistant {
        node_id: NodeId,
        source: String,
        document: crate::MarkdownDocument,
        status: String,
    },
    Plan {
        node_id: NodeId,
        source: String,
        document: crate::MarkdownDocument,
        status: String,
    },
    AcceptedPlan {
        node_id: NodeId,
        source: String,
        document: crate::MarkdownDocument,
        clear_context: bool,
        compact_context: bool,
        compaction_summary: Option<String>,
    },
    Compacted {
        node_id: NodeId,
        summary: String,
    },
    Tool {
        node_id: NodeId,
        name: String,
        arguments: Value,
        result: Option<Value>,
        failed: bool,
    },
    Notice {
        node_id: NodeId,
        message: String,
    },
    Recap {
        node_id: NodeId,
        text: String,
    },
    WorkspaceTransition {
        node_id: NodeId,
        label: String,
        target: String,
        base: Option<String>,
        path: Option<PathBuf>,
    },
    SystemLog {
        cursor: EventCursor,
        message: String,
    },
    PermissionDenied {
        node_id: NodeId,
        resource: String,
        reason: String,
    },
    Interrupt {
        node_id: NodeId,
        queued_steering: bool,
    },
}

/// Stable identity for a semantic transcript block.
///
/// Node-backed blocks use the durable node ID. Projected groups use a stable
/// descriptive ID based on their first durable node, so frontends can
/// reconcile a changing live block without inferring its position.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TranscriptBlockId(pub String);

impl TranscriptBlockId {
    #[must_use]
    pub fn node(node_id: NodeId) -> Self {
        Self(node_id.to_string())
    }

    #[must_use]
    pub fn derived(prefix: &str, node_id: NodeId) -> Self {
        Self(format!("{prefix}:{node_id}"))
    }

    #[must_use]
    pub fn event(cursor: EventCursor) -> Self {
        Self(format!("event:{}", cursor.0))
    }
}

/// Lifecycle state supplied by the agent for a transcript block.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptBlockStatus {
    Pending,
    Streaming,
    Completed,
    Failed,
    Cancelled,
}

/// A complete color- and width-neutral transcript item.
///
/// Grouping, ordering, live content, and lifecycle are agent-owned. A
/// frontend may cache layout by `id`, but must not recreate transcript policy.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptBlock {
    pub id: TranscriptBlockId,
    pub status: TranscriptBlockStatus,
    pub kind: TranscriptBlockKind,
}

/// Opaque continuation for transcript content older than a window.
///
/// Cursors are scoped to one conversation branch boundary. Frontends must
/// pass them back unchanged and treat [`RuntimeError::StaleTranscriptCursor`]
/// as a signal to reattach.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TranscriptCursor {
    pub(crate) conversation_id: ConversationId,
    pub(crate) older_node_id: NodeId,
    pub(crate) newer_node_id: NodeId,
}

/// An ordered, bounded portion of the visible transcript.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranscriptWindow {
    pub blocks: Vec<TranscriptBlock>,
    /// Continuation for content immediately before `blocks`.
    pub older: Option<TranscriptCursor>,
}

impl TranscriptWindow {
    #[must_use]
    pub fn new(blocks: Vec<TranscriptBlock>, older: Option<TranscriptCursor>) -> Self {
        Self { blocks, older }
    }

    pub fn push(&mut self, block: TranscriptBlock) {
        self.blocks.push(block);
    }

    /// Prepends an older page while rejecting an out-of-order response and
    /// deduplicating any semantic groups overfetched at the page boundary.
    pub fn prepend(&mut self, requested: &TranscriptCursor, page: TranscriptPage) -> bool {
        if self.older.as_ref() != Some(requested) {
            return false;
        }
        let existing = self
            .blocks
            .iter()
            .map(|block| block.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut prefix = page
            .blocks
            .into_iter()
            .filter(|block| !existing.contains(&block.id))
            .collect::<Vec<_>>();
        prefix.append(&mut self.blocks);
        self.blocks = prefix;
        self.older = page.older;
        true
    }

    /// Reconciles a fresh tail without discarding already loaded prefixes.
    /// Returns false when there is no stable overlap and the caller must use
    /// the fresh tail as a branch reset.
    pub fn reconcile_tail(&mut self, mut tail: TranscriptWindow) -> bool {
        let Some(overlap) = tail.blocks.iter().position(|candidate| {
            !transcript_id_is_ephemeral(&candidate.id)
                && self.blocks.iter().any(|block| block.id == candidate.id)
        }) else {
            *self = tail;
            return false;
        };
        let overlap_id = tail.blocks[overlap].id.clone();
        let Some(existing_overlap) = self.blocks.iter().position(|block| block.id == overlap_id)
        else {
            unreachable!("overlap was selected from the existing window")
        };
        let matching = self.blocks[existing_overlap..]
            .iter()
            .zip(&tail.blocks[overlap..])
            .take_while(|(existing, candidate)| existing.id == candidate.id)
            .count();
        if self.blocks[existing_overlap + matching..]
            .iter()
            .any(|block| !transcript_id_is_ephemeral(&block.id))
        {
            *self = tail;
            return false;
        }
        self.blocks.truncate(existing_overlap);
        self.blocks.append(&mut tail.blocks);
        // A loaded prefix owns the continuation. A pure tail window uses the
        // continuation supplied by the newest snapshot.
        if existing_overlap == 0 {
            self.older = tail.older;
        }
        true
    }
}

fn transcript_id_is_ephemeral(id: &TranscriptBlockId) -> bool {
    // Detached terminal cards are appended after the live tail, not ordered
    // on the active ancestry. New assistant/tool blocks can appear before
    // them without changing branches. Nor can their overlap prove continuity.
    matches!(
        id.0.as_str(),
        "work" | "live-assistant" | "live-plan" | "pending-interrupt"
    ) || id.0.starts_with("detached-bash:")
}

impl std::ops::Deref for TranscriptWindow {
    type Target = [TranscriptBlock];

    fn deref(&self) -> &Self::Target {
        &self.blocks
    }
}

impl std::ops::DerefMut for TranscriptWindow {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.blocks
    }
}

impl IntoIterator for TranscriptWindow {
    type Item = TranscriptBlock;
    type IntoIter = std::vec::IntoIter<TranscriptBlock>;

    fn into_iter(self) -> Self::IntoIter {
        self.blocks.into_iter()
    }
}

impl<'a> IntoIterator for &'a TranscriptWindow {
    type Item = &'a TranscriptBlock;
    type IntoIter = std::slice::Iter<'a, TranscriptBlock>;

    fn into_iter(self) -> Self::IntoIter {
        self.blocks.iter()
    }
}

impl<'a> IntoIterator for &'a mut TranscriptWindow {
    type Item = &'a mut TranscriptBlock;
    type IntoIter = std::slice::IterMut<'a, TranscriptBlock>;

    fn into_iter(self) -> Self::IntoIter {
        self.blocks.iter_mut()
    }
}

impl From<Vec<TranscriptBlock>> for TranscriptWindow {
    fn from(blocks: Vec<TranscriptBlock>) -> Self {
        Self::new(blocks, None)
    }
}

/// One page returned by [`SessionHandle::load_older_transcript`](crate::SessionHandle::load_older_transcript).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranscriptPage {
    pub blocks: Vec<TranscriptBlock>,
    pub older: Option<TranscriptCursor>,
}

/// Semantic data for one transcript block.
#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptBlockKind {
    User {
        /// A user-facing command label, never included in model input.
        label: Option<String>,
        text: String,
        attachments: Vec<crate::presentation::SubmittedAttachment>,
        images: Vec<ImageAttachment>,
        image_chips: Vec<ImageChipRange>,
    },
    Assistant {
        source: String,
        document: crate::MarkdownDocument,
        message: Option<String>,
    },
    Plan {
        source: String,
        document: crate::MarkdownDocument,
    },
    AcceptedPlan {
        source: String,
        document: crate::MarkdownDocument,
        clear_context: bool,
        compact_context: bool,
        compaction_summary: Option<String>,
    },
    /// A completed context checkpoint and the summary carried into later requests.
    Compacted {
        summary: String,
    },
    ToolGroups {
        groups: Vec<crate::presentation::ToolActivityGroup>,
    },
    Edits {
        diff: crate::tools::SemanticDiff,
    },
    Notice {
        message: String,
    },
    /// A small-model summary generated after the conversation becomes idle.
    Recap {
        text: String,
    },
    WorkspaceTransition {
        label: String,
        target: String,
        base: Option<String>,
        /// Resolved destination used by frontends for directory navigation.
        /// This is optional so older persisted transcript entries remain valid.
        path: Option<PathBuf>,
    },
    PermissionDenied {
        resource: String,
        reason: String,
    },
    Interrupt {
        queued_steering: bool,
    },
    Work {
        state: TurnState,
    },
}

/// Atomic core state and tail transcript window for an active session.
/// Catalog, history-tree, full-history, and older transcript-page queries
/// intentionally remain separate APIs.
#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub conversation_id: ConversationId,
    /// Immutable launch directory and canonical history/trust scope.
    pub project_dir: PathBuf,
    /// Mutable session working directory used by newly started work.
    pub cwd: PathBuf,
    pub worktree: Option<WorktreeMetadata>,
    pub access: SessionAccess,
    pub cursor: Option<EventCursor>,
    pub title: Option<String>,
    /// Ordered, renderer-ready transcript tail plus its older continuation.
    /// This is the sole source of transcript and active-turn state for
    /// frontends.
    pub transcript: TranscriptWindow,
    pub turn: TurnState,
    /// Latest transient checklist supplied by `update_plan` for the active
    /// primary turn. It is absent once that turn ends.
    pub active_plan: Option<UpdatePlanArgs>,
    /// Unix-millisecond timestamp of the last observable primary-turn
    /// activity. It is absent while no runtime-owned turn is active.
    pub last_activity_at: Option<String>,
    pub model_selection: Option<(String, String, Option<String>)>,
    /// Persisted global Fast preference.
    pub fast: bool,
    /// True when Fast is configured and the active model advertises it.
    pub fast_effective: bool,
    pub active_agent: String,
    pub active_mode: String,
    pub queue: Vec<QueuedMessage>,
    pub composer_history: Vec<ComposerHistoryEntry>,
    pub usage: SessionUsage,
    pub context: Option<ContextUsage>,
    /// Transient account limits reported by the active model provider.
    pub provider_usage: Option<crate::ProviderUsageReport>,
    pub pending_interaction: Option<InteractionRequest>,
    pub agent_runs: Vec<AgentRun>,
    /// Live delegated-run details retained independently of durable run events.
    pub delegated_live: Vec<crate::presentation::DelegatedRunLive>,
    pub terminals: Vec<crate::TerminalSnapshot>,
    /// Agent-projected rows for the background-work browser, including every
    /// supervised terminal and delegated run.
    pub supervised_work: Vec<crate::presentation::SupervisedWork>,
    pub enabled_providers: Vec<String>,
    pub startup_resources: StartupResourceStatus,
}

/// Complete replacement snapshot accepted by the `update_plan` tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlanArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub plan: Vec<PlanItemArg>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanItemArg {
    pub step: String,
    pub status: PlanStepStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

/// Immutable transcript projection ending at a durable history target.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryPreviewSnapshot {
    pub target: NodeId,
    pub transcript: TranscriptWindow,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionAccess {
    Owner,
    Observer { takeover_available: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextUsage {
    pub used_tokens: u64,
    pub context_window: u64,
}

impl ContextUsage {
    /// Whole percentage of the current request context, rounded to nearest and
    /// clamped for provider reports that exceed the advertised window.
    #[must_use]
    pub fn percent_used(&self) -> Option<u64> {
        (self.context_window > 0).then(|| {
            let used = u128::from(self.used_tokens);
            let window = u128::from(self.context_window);
            ((used * 100 + window / 2) / window).min(100) as u64
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateDurability {
    Durable,
    Transient,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // Update payloads are short-lived and sent directly to subscribers.
pub enum SessionUpdateKind {
    Snapshot(SessionSnapshot),
    /// Ephemeral provider retry backoff state for the active turn.
    RetryScheduled(RetryStatusUpdate),
    /// The selected provider's transient usage report changed without
    /// affecting durable session or transcript state.
    ProviderUsage(Option<crate::ProviderUsageReport>),
    /// Startup-resource readiness changed without affecting durable session or
    /// transcript state.
    StartupResources(StartupResourceStatus),
    /// A committed supervised-terminal change. Unlike a snapshot this touches
    /// one stable live transcript activity and carries only its bounded
    /// preview, so high-volume PTY output cannot rehydrate the transcript.
    Terminal(TerminalTranscriptUpdate),
    /// A committed delegated-terminal change. Delegated terminals are owned
    /// by an agent run rather than a conversation node, so they update the
    /// delegated log and supervised-work projections without a transcript ID.
    DelegatedTerminal(crate::tools::TerminalSnapshot),
    /// Lifecycle metadata for one delegated run. Durable log entries are
    /// fetched separately through `SessionHandle::agent_run_log_page`.
    DelegatedRun(DelegatedRunUpdate),
    /// Ephemeral provider text for one active delegated run.
    DelegatedText(DelegatedTextUpdate),
    /// The subscriber fell behind. The attachment must be replaced with a new
    /// call to `SessionHandle::attach` before relying on state again.
    ResyncRequired,
}

#[derive(Clone, Debug)]
pub struct DelegatedRunUpdate {
    pub run: AgentRun,
}

#[derive(Clone, Debug)]
pub struct DelegatedTextUpdate {
    pub id: AgentRunId,
    pub text: String,
}

/// A bounded, chronological page of a delegated run's durable log.
/// Pass `next_after` back unchanged to read the next page (or later appends).
#[derive(Clone, Debug)]
pub struct AgentRunLogPage {
    pub entries: Vec<AgentRunTimelineEntry>,
    pub next_after: u64,
    pub has_more: bool,
}

/// Semantic provider retry status for frontend reconnect indicators.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryStatusUpdate {
    pub request_id: RequestId,
    pub next_attempt: u32,
    pub max_attempts: u32,
    pub reason: String,
    pub delay_millis: u64,
}

/// Incremental state for one Bash transcript activity.
#[derive(Clone, Debug)]
pub struct TerminalTranscriptUpdate {
    /// Stable identity for this terminal activity. The owning transcript card
    /// is located through `terminal.tool_call_node_id` when it is grouped with
    /// neighboring tools.
    pub id: TranscriptBlockId,
    pub terminal: crate::tools::TerminalSnapshot,
}

/// A semantic notification attached to the snapshot that caused it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionUpdateNotice {
    /// The active turn and all queued follow-up dispatches completed normally.
    TurnCompleted,
    /// A managed provider authentication attempt reached a terminal state.
    ProviderAuthUpdated,
}

#[derive(Clone, Debug)]
pub struct SessionUpdate {
    pub origin: Option<CommandId>,
    pub cursor: Option<EventCursor>,
    pub durability: UpdateDurability,
    pub notice: Option<SessionUpdateNotice>,
    /// The navigation projection may have changed and open history surfaces
    /// should refresh their cached rows.
    pub history_changed: bool,
    pub kind: SessionUpdateKind,
}

impl SessionSnapshot {
    /// Applies a semantic update. Keeping this reducer in the agent crate
    /// prevents frontends from independently reconstructing session state.
    pub fn apply(&mut self, update: SessionUpdate) -> bool {
        match update.kind {
            SessionUpdateKind::Snapshot(mut snapshot) => {
                if self.conversation_id == snapshot.conversation_id {
                    let mut retained = std::mem::take(&mut self.transcript);
                    retained.reconcile_tail(snapshot.transcript);
                    snapshot.transcript = retained;
                }
                *self = snapshot;
                true
            }
            // Retry status is intentionally not part of the durable snapshot.
            SessionUpdateKind::RetryScheduled(_) => true,
            SessionUpdateKind::ProviderUsage(report) => {
                self.provider_usage = report;
                true
            }
            SessionUpdateKind::StartupResources(status) => {
                self.startup_resources = status;
                true
            }
            SessionUpdateKind::Terminal(update) => {
                let terminal = update.terminal;
                self.upsert_terminal(terminal.clone());
                let detached = update.id.0.starts_with("detached-bash:");
                if detached {
                    let block = crate::presentation::detached_bash_transcript_block(&terminal);
                    if let Some(existing) = self
                        .transcript
                        .iter_mut()
                        .find(|existing| existing.id == update.id)
                    {
                        *existing = block;
                    } else {
                        self.transcript.push(block);
                    }
                }
                // Delegated terminals belong exclusively to the child run's
                // log. The primary transcript only owns foreground Bash
                // terminals; the snapshot still retains every terminal for
                // supervised-work and delegated-log consumers.
                if !detached
                    && let Some(node_id) = terminal.tool_call_node_id
                    && !crate::presentation::terminal_belongs_to_any_agent(
                        &terminal,
                        &self.agent_runs,
                    )
                {
                    let result = crate::presentation::terminal_activity_result(&terminal);
                    let status = crate::presentation::terminal_activity_status(&terminal);
                    for block in &mut self.transcript {
                        let TranscriptBlockKind::ToolGroups { groups } = &mut block.kind else {
                            continue;
                        };
                        for group in groups {
                            crate::presentation::update_projected_bash_activity(
                                group, node_id, &result, status,
                            );
                        }
                    }
                }
                true
            }
            SessionUpdateKind::DelegatedTerminal(terminal) => {
                self.upsert_terminal(terminal);
                true
            }
            SessionUpdateKind::DelegatedRun(update) => {
                let run = update.run;
                if let Some(existing) = self.agent_runs.iter_mut().find(|item| item.id == run.id) {
                    *existing = run.clone();
                } else {
                    self.agent_runs.push(run.clone());
                }
                if let Some(existing) = self.supervised_work.iter_mut().find(|item| {
                    matches!(item, crate::presentation::SupervisedWork::Agent { run: item } if item.id == run.id)
                }) {
                    *existing = crate::presentation::SupervisedWork::Agent {
                        run: Box::new(run.clone()),
                    };
                } else {
                    self.supervised_work.push(crate::presentation::SupervisedWork::Agent {
                        run: Box::new(run.clone()),
                    });
                    crate::presentation::sort_supervised_work_newest_first(&mut self.supervised_work);
                }
                let live = self
                    .delegated_live
                    .iter_mut()
                    .find(|item| item.id == run.id);
                if let Some(live) = live {
                    live.usage.clone_from(&run.usage);
                } else if !run.status.is_terminal() {
                    self.delegated_live
                        .push(crate::presentation::DelegatedRunLive {
                            id: run.id,
                            text: String::new(),
                            usage: run.usage.clone(),
                        });
                }
                if run.status.is_terminal() {
                    self.delegated_live.retain(|item| item.id != run.id);
                }
                true
            }
            SessionUpdateKind::DelegatedText(update) => {
                if let Some(live) = self
                    .delegated_live
                    .iter_mut()
                    .find(|item| item.id == update.id)
                {
                    live.text = update.text;
                } else {
                    self.delegated_live
                        .push(crate::presentation::DelegatedRunLive {
                            id: update.id,
                            text: update.text,
                            usage: None,
                        });
                }
                true
            }
            SessionUpdateKind::ResyncRequired => false,
        }
    }

    fn upsert_terminal(&mut self, terminal: crate::tools::TerminalSnapshot) {
        if let Some(existing) = self
            .terminals
            .iter_mut()
            .find(|item| item.id == terminal.id)
        {
            *existing = terminal;
        } else {
            self.terminals.push(terminal);
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionAction {
    SubmitDraft {
        draft: UserDraft,
    },
    QueueDraft {
        draft: UserDraft,
        target: QueueTarget,
    },
    ReplaceQueuedDraft {
        id: QueuedMessageId,
        draft: UserDraft,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<QueueTarget>,
    },
    SubmitInput {
        text: String,
    },
    /// Launches an explicit user Bash command without starting an agent turn.
    RunBash {
        command: String,
    },
    /// Submit a prompt whose next model turn must delegate at least one subtask.
    SubmitSpawnInput {
        text: String,
    },
    /// Submit a prompt whose next model turn must call web_search before answering.
    SubmitWebSearchInput {
        text: String,
    },
    SubmitStructuredInput {
        text: String,
        schema: Value,
    },
    SubmitExecInput {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema: Option<Value>,
        tool_policy: ToolPolicy,
    },
    SubmitWithAttachments {
        text: String,
        attachments: Vec<AttachmentSpec>,
    },
    SubmitSpawnWithAttachments {
        text: String,
        attachments: Vec<AttachmentSpec>,
    },
    SubmitWebSearchWithAttachments {
        text: String,
        attachments: Vec<AttachmentSpec>,
    },
    QueueInput {
        text: String,
        target: QueueTarget,
    },
    QueueInputWithAttachments {
        text: String,
        target: QueueTarget,
        attachments: Vec<AttachmentSpec>,
    },
    /// Queues a direct mode command and its prompt without changing the active mode yet.
    QueueModeInputWithAttachments {
        mode: String,
        command_text: String,
        text: String,
        target: QueueTarget,
        attachments: Vec<AttachmentSpec>,
    },
    /// Temporarily prevents this queued item and all later items from being
    /// dispatched while a frontend edits it.
    BeginEditingQueued {
        id: QueuedMessageId,
    },
    ReplaceQueued {
        id: QueuedMessageId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<QueueTarget>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<AttachmentSpec>,
    },
    /// Replaces a queued item with a direct mode command and its prompt.
    ReplaceQueuedModeInput {
        id: QueuedMessageId,
        mode: String,
        command_text: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<QueueTarget>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<AttachmentSpec>,
    },
    DeleteQueued {
        id: QueuedMessageId,
    },
    PromoteQueued {
        id: QueuedMessageId,
    },
    /// Compact the active branch immediately when idle, or enqueue the action
    /// at the requested boundary while another turn is active.
    Compact {
        target: QueueTarget,
        /// Optional user-provided instructions for this manual compaction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    /// Immediately generate and persist a recap of the recent conversation.
    Recap,
    Cancel,
    /// Stops the session without dispatching queued work. Frontends use this
    /// before leaving a conversation so its active node cannot continue in
    /// the background.
    End,
    ChangeModel {
        provider: String,
        model: String,
    },
    /// Selects a model for this session without changing global configuration defaults.
    SetSessionModel {
        provider: String,
        model: String,
        effort: Option<String>,
    },
    /// Selects a model for one headless exec run. This may use a disabled
    /// provider only after the frontend verified environment credentials.
    SetSessionExecModel {
        provider: String,
        model: String,
        effort: Option<String>,
    },
    /// Selects a model for a specific mode without changing global defaults.
    SetModeModel {
        mode: String,
        provider: String,
        model: String,
        effort: Option<String>,
    },
    ChangeModelAndEffort {
        provider: String,
        model: String,
        effort: Option<String>,
    },
    ChangeEffort {
        effort: String,
    },
    /// Clears the current mode's manual marker and re-resolves its inherited profile default.
    UseAgentDefault,
    ChangeAgent {
        agent: String,
    },
    ChangeMode {
        mode: String,
    },
    RenameConversation {
        title: String,
    },
    Fork {
        at: NodeId,
    },
    Retry,
    /// Re-reads local configuration and instructions, then refreshes model catalogs in the
    /// background.
    ReloadStartupResources,
    RespondToInteraction {
        request_id: InteractionRequestId,
        response: Value,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    ConversationRoot,
    UserMessage,
    AssistantMessage,
    AcceptedPlan,
    /// Durable boundary that discards earlier ancestry when reconstructing model input.
    /// A successful branch-local provider-context checkpoint. It is omitted
    /// from the ordinary transcript but remains visible and forkable in history.
    CompactionSummary,
    ToolCall,
    ToolResult,
    PermissionDecision,
    /// Runtime-authored model context. Frontends omit these nodes from the user transcript.
    System,
}

/// Durable contents of an accepted plan. One node owns both model messages:
/// the implementation instruction followed by the plan Markdown.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcceptedPlanPayload {
    pub instruction: String,
    pub plan_markdown: String,
    pub reset_context: bool,
    #[serde(default)]
    pub compact_context: bool,
    pub destination: AcceptedPlanDestination,
    pub reset_context_window: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcceptedPlanDestination {
    pub agent: String,
    pub mode: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Branch-positioned durable system behavior. Operational diagnostics and
/// provider retry state are deliberately not represented here.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "system_type", rename_all = "snake_case")]
pub enum SystemNodePayload {
    LocalModelContext {
        message: String,
        snapshot: crate::LocalContextSnapshot,
    },
    CompletionEnvelopes {
        envelopes: Vec<Value>,
    },
    TranscriptNotice {
        message: String,
    },
    Recap {
        text: String,
    },
    Interruption {
        queued_steering: bool,
    },
    WorkspaceTransition {
        message: String,
        transition: Value,
    },
    SettingsChange {
        message: String,
    },
    TitleChange {
        title: String,
        source: ConversationTitleSource,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    Manual,
    Automatic,
    OverflowRecovery,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanContextAction {
    #[default]
    Keep,
    Clear,
    Compact,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactionModelSettings {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
}

/// Versioned durable content of a completed compaction checkpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactionSummary {
    pub version: u16,
    pub summary: String,
    pub retained_from_node_id: Option<NodeId>,
    pub trigger: CompactionTrigger,
    pub estimated_input_tokens: u64,
    pub context_window_tokens: u64,
    pub threshold_percent: u8,
    pub summary_model: CompactionModelSettings,
    pub agent: String,
    pub mode: String,
    pub source_tip_node_id: NodeId,
    /// Model-visible node deliberately omitted from this checkpoint's source.
    /// Used when a proposed plan is compacted around and then re-applied fresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_node_id: Option<NodeId>,
    /// Accepted plan replayed after this checkpoint's summarized prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reapplied_node_id: Option<NodeId>,
    pub previous_checkpoint_id: Option<NodeId>,
    /// Last raw node incorporated into the summary rather than retained verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_summarized_node_id: Option<NodeId>,
    /// Estimated size of the raw suffix retained verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_token_estimate: Option<u64>,
    /// Preferred raw-suffix budget used when this checkpoint was produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_token_target: Option<u64>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DurableEventKind {
    NodeAppended {
        node_id: NodeId,
        parent_id: Option<NodeId>,
        #[serde(default)]
        turn_id: Option<TurnId>,
        #[serde(default)]
        owner_id: Option<NodeId>,
        #[serde(default)]
        request_index: Option<u64>,
        node_kind: NodeKind,
        status: String,
        content: Value,
    },
    AssistantDelta {
        node_id: NodeId,
        delta: String,
        /// Agent-owned semantic projection of the assistant text through this
        /// committed delta. This is derived after replay and is intentionally
        /// not duplicated in `SQLite`'s raw durable event log.
        #[serde(skip)]
        document: crate::MarkdownDocument,
    },
    /// The streaming assistant placeholder became a proposed plan. The node
    /// ID deliberately remains stable for the lifetime of the response.
    PlanStarted {
        node_id: NodeId,
    },
    /// A durably committed proposed-plan Markdown delta.
    PlanDelta {
        node_id: NodeId,
        delta: String,
        #[serde(skip)]
        document: crate::MarkdownDocument,
    },
    AssistantFailed {
        node_id: NodeId,
        code: String,
        message: String,
        retryable: bool,
    },
    ModelUsageRecorded {
        node_id: NodeId,
        usage: crate::ModelUsage,
    },
    ActiveNodeChanged {
        node_id: NodeId,
    },
    NodeStatusChanged {
        node_id: NodeId,
        status: String,
    },
    QueuedInputCreated {
        message: QueuedMessage,
    },
    QueuedInputReplaced {
        message: QueuedMessage,
    },
    QueuedInputDeleted {
        id: QueuedMessageId,
    },
    QueuedInputPromoted {
        message: QueuedMessage,
    },
    QueuedInputDispatched {
        id: QueuedMessageId,
        node_id: NodeId,
    },
    ModelSelectionChanged {
        provider: String,
        model: String,
        effort: Option<String>,
        pending: bool,
        /// Active branch position where this selection became effective.
        #[serde(default)]
        node_id: Option<NodeId>,
    },
    AgentChanged {
        agent: String,
        pending: bool,
        /// Active branch position where this selection became effective.
        #[serde(default)]
        node_id: Option<NodeId>,
    },
    ModeChanged {
        mode: String,
        pending: bool,
    },
    ConversationTitleChanged {
        title: String,
        source: ConversationTitleSource,
        /// Active branch position where this title became effective.
        #[serde(default)]
        node_id: Option<NodeId>,
    },
    /// Non-transcript audit record emitted after a valid configuration snapshot is published.
    ConfigurationChanged {
        path: Option<PathBuf>,
    },
}

impl DurableEventKind {
    /// Event category retained in the transcript paging index. Streaming
    /// deltas are deliberately absent because the node row stores their
    /// finalized aggregate.
    #[must_use]
    pub const fn transcript_index_kind(&self) -> Option<&'static str> {
        match self {
            Self::NodeAppended { .. } => Some("node_appended"),
            Self::AssistantFailed { .. } => Some("assistant_failed"),
            Self::ModelSelectionChanged { .. } => Some("model_selection_changed"),
            Self::AgentChanged { .. } => Some("agent_changed"),
            Self::ConversationTitleChanged { .. } => Some("conversation_title_changed"),
            _ => None,
        }
    }

    /// Durable node used to index transcript projection events.
    #[must_use]
    pub const fn transcript_node(&self) -> Option<NodeId> {
        match self {
            Self::NodeAppended {
                node_id,
                parent_id,
                node_kind: NodeKind::System,
                ..
            } => match parent_id {
                Some(parent_id) => Some(*parent_id),
                None => Some(*node_id),
            },
            Self::NodeAppended { node_id, .. }
            | Self::AssistantDelta { node_id, .. }
            | Self::PlanStarted { node_id }
            | Self::PlanDelta { node_id, .. }
            | Self::AssistantFailed { node_id, .. }
            | Self::ModelUsageRecorded { node_id, .. }
            | Self::ActiveNodeChanged { node_id }
            | Self::NodeStatusChanged { node_id, .. }
            | Self::QueuedInputDispatched { node_id, .. } => Some(*node_id),
            Self::ModelSelectionChanged { node_id, .. }
            | Self::AgentChanged { node_id, .. }
            | Self::ConversationTitleChanged { node_id, .. } => *node_id,
            _ => None,
        }
    }

    /// Returns the branch position for transcript-only system log entries.
    #[must_use]
    pub const fn system_log_node(&self) -> Option<NodeId> {
        match self {
            Self::ModelSelectionChanged { node_id, .. }
            | Self::AgentChanged { node_id, .. }
            | Self::ConversationTitleChanged { node_id, .. } => *node_id,
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_branch_system_log(&self) -> bool {
        matches!(
            self,
            Self::ModelSelectionChanged { .. }
                | Self::AgentChanged { .. }
                | Self::ConversationTitleChanged { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationTitleSource {
    Generated,
    Fallback,
    Manual,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DurableEvent {
    pub version: u16,
    pub cursor: EventCursor,
    pub conversation_id: ConversationId,
    pub kind: DurableEventKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InteractionRequest {
    pub id: InteractionRequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<InteractionOrigin>,
    pub kind: InteractionRequestKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutoReviewSummary {
    pub decision: String,
    pub risk: String,
    pub authorization: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InteractionOrigin {
    SubAgent { id: AgentRunId, profile: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum InteractionRequestKind {
    Question {
        questions: Vec<crate::QuestionPrompt>,
    },
    PermissionApproval {
        resource: crate::PermissionResource,
        decision: crate::FilesystemPermissionDecision,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queued_message_id: Option<QueuedMessageId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<crate::SemanticDiff>,
        /// Original MCP call arguments, shown only for external-tool approvals.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<Value>,
        /// Optional frontend-neutral annotation from auto review.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto_review: Option<AutoReviewSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suggested_rule: Option<crate::PermissionRule>,
    },
    PlanCompletion {
        plan: String,
        implementation_modes: Vec<String>,
        default_mode: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransientEvent {
    /// The authoritative composer history in the session snapshot changed.
    ComposerHistoryUpdated,
    ConfigurationChanged {
        path: Option<PathBuf>,
    },
    ConfigurationRejected {
        path: PathBuf,
        message: String,
    },
    LocalContextReloadFailed {
        path: PathBuf,
        message: String,
    },
    Working,
    /// Cancellation was requested for the current turn. This lets frontends
    /// stop presenting the turn before durable cleanup completes.
    TurnCancellationRequested,
    /// The current turn stopped because of an unexpected runtime failure.
    /// This is intentionally transient: the durable transcript may already
    /// contain the completed portion of the turn, and the session remains
    /// available for another command.
    TurnFailed {
        message: String,
    },
    /// The active turn and any queued follow-up dispatches have completed.
    TurnCompleted,
    RetryScheduled {
        request_id: RequestId,
        attempt: u32,
        reason: String,
        delay_millis: u64,
    },
    ContextUpdated {
        used_tokens: u64,
        context_window: u64,
    },
    /// The active provider's transient account usage report changed.
    ProviderUsageUpdated,
    ModelCapabilityNotice {
        provider: String,
        model: String,
        message: String,
    },
    QueuedAttachmentPaused {
        id: QueuedMessageId,
        message: String,
    },
    ModelCatalogUpdated {
        provider: String,
        catalog: crate::ResolvedModelCatalog,
    },
    ProviderAuthUpdated {
        provider: String,
        auth: crate::AuthState,
    },
    McpCatalogUpdated {
        servers: Vec<crate::McpEffectiveServer>,
    },
    McpStatusUpdated {
        server: String,
        status: crate::McpRuntimeStatus,
    },
    StartupResourcesUpdated {
        status: StartupResourceStatus,
    },
    AgentRunUpdated {
        run: Box<crate::AgentRun>,
    },
    /// Ephemeral text from the current provider continuation of a delegated run.
    AgentRunTextUpdated {
        id: AgentRunId,
        text: String,
    },
    /// A locally validated final assistant value requested by headless exec.
    StructuredOutput {
        value: Value,
    },
    TerminalUpdated {
        terminal: Box<crate::TerminalSnapshot>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "durability", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // Wire events preserve their direct serialized shape.
pub enum RuntimeEvent {
    Durable(DurableEvent),
    Transient {
        version: u16,
        event: TransientEvent,
    },
    Interaction {
        version: u16,
        request: Box<InteractionRequest>,
    },
    /// The interaction watch transitioned from a pending request to no
    /// request. Frontends use this event to refresh the complete snapshot so
    /// transcript items hidden behind an approval become visible again.
    InteractionCleared {
        version: u16,
    },
}

#[cfg(test)]
mod transcript_window_tests {
    use super::*;

    #[test]
    fn history_nodes_convert_to_append_events_without_losing_event_fields() {
        let conversation_id = ConversationId::new();
        let parent_id = NodeId::new();
        let turn_id = TurnId::new();
        let owner_id = NodeId::new();
        let kinds = [
            NodeKind::ConversationRoot,
            NodeKind::UserMessage,
            NodeKind::AssistantMessage,
            NodeKind::AcceptedPlan,
            NodeKind::CompactionSummary,
            NodeKind::ToolCall,
            NodeKind::ToolResult,
            NodeKind::PermissionDecision,
            NodeKind::System,
        ];
        for (index, kind) in kinds.into_iter().enumerate() {
            let node_id = NodeId::new();
            let content = serde_json::json!({ "kind_index": index });
            let event = HistoryNode {
                id: node_id,
                parent_id: Some(parent_id),
                turn_id: Some(turn_id),
                owner_id: Some(owner_id),
                request_index: Some(index as u64),
                kind: kind.clone(),
                status: NodeStatus::Completed,
                role: Some("ignored persistence field".into()),
                summary: Some("ignored persistence field".into()),
                content: content.clone(),
                composer_text: None,
                created_at: "2025-01-01T00:00:00Z".into(),
                completed_at: Some("2025-01-01T00:00:01Z".into()),
                active: index == 0,
            }
            .into_appended_event(conversation_id, EventCursor(index as u64));
            assert_eq!(event.conversation_id, conversation_id);
            assert_eq!(event.cursor, EventCursor(index as u64));
            let DurableEventKind::NodeAppended {
                node_id: projected_id,
                parent_id: projected_parent,
                turn_id: projected_turn,
                owner_id: projected_owner,
                request_index,
                node_kind,
                status,
                content: projected_content,
            } = event.kind
            else {
                panic!("history nodes must produce append events");
            };
            assert_eq!(projected_id, node_id);
            assert_eq!(projected_parent, Some(parent_id));
            assert_eq!(projected_turn, Some(turn_id));
            assert_eq!(projected_owner, Some(owner_id));
            assert_eq!(request_index, Some(index as u64));
            assert_eq!(node_kind, kind);
            assert_eq!(status, "completed");
            assert_eq!(projected_content, content);
        }
    }

    fn block(id: &str) -> TranscriptBlock {
        TranscriptBlock {
            id: TranscriptBlockId(id.into()),
            status: TranscriptBlockStatus::Completed,
            kind: TranscriptBlockKind::Notice { message: id.into() },
        }
    }

    fn cursor(conversation_id: ConversationId) -> TranscriptCursor {
        TranscriptCursor {
            conversation_id,
            older_node_id: NodeId::new(),
            newer_node_id: NodeId::new(),
        }
    }

    #[test]
    fn prepend_is_ordered_deduplicated_and_cursor_guarded() {
        let conversation_id = ConversationId::new();
        let requested = cursor(conversation_id);
        let next = cursor(conversation_id);
        let mut window =
            TranscriptWindow::new(vec![block("b"), block("c")], Some(requested.clone()));
        assert!(!window.prepend(
            &cursor(conversation_id),
            TranscriptPage {
                blocks: vec![block("ignored")],
                older: None,
            },
        ));
        assert!(window.prepend(
            &requested,
            TranscriptPage {
                blocks: vec![block("a"), block("b")],
                older: Some(next.clone()),
            },
        ));
        assert_eq!(
            window
                .iter()
                .map(|block| block.id.0.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(window.older, Some(next));
    }

    #[test]
    fn tail_reconciliation_retains_a_loaded_prefix() {
        let conversation_id = ConversationId::new();
        let older = cursor(conversation_id);
        let mut window = TranscriptWindow::new(
            vec![block("a"), block("b"), block("c")],
            Some(older.clone()),
        );
        assert!(window.reconcile_tail(TranscriptWindow::new(
            vec![block("b"), block("c"), block("d")],
            Some(cursor(conversation_id)),
        )));
        assert_eq!(
            window
                .iter()
                .map(|block| block.id.0.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
        assert_eq!(window.older, Some(older));
    }

    #[test]
    fn tail_reconciliation_resets_on_durable_branch_divergence() {
        let conversation_id = ConversationId::new();
        let next = cursor(conversation_id);
        let mut window = TranscriptWindow::new(
            vec![block("a"), block("b"), block("abandoned")],
            Some(cursor(conversation_id)),
        );
        assert!(!window.reconcile_tail(TranscriptWindow::new(
            vec![block("a"), block("b"), block("fork")],
            Some(next.clone()),
        )));
        assert_eq!(
            window
                .iter()
                .map(|block| block.id.0.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "fork"]
        );
        assert_eq!(window.older, Some(next));
    }

    #[test]
    fn tail_reconciliation_keeps_loaded_history_when_live_text_precedes_detached_work() {
        let conversation_id = ConversationId::new();
        let older = cursor(conversation_id);
        let mut window = TranscriptWindow::new(
            vec![
                block("loaded-prefix"),
                block("tail"),
                block("work"),
                block("detached-bash:1"),
            ],
            Some(older.clone()),
        );
        assert!(window.reconcile_tail(TranscriptWindow::new(
            vec![
                block("tail"),
                block("live-assistant"),
                block("work"),
                block("detached-bash:1")
            ],
            Some(cursor(conversation_id)),
        )));
        assert_eq!(window.first().unwrap().id.0, "loaded-prefix");
        assert_eq!(window.older, Some(older));
    }

    #[test]
    fn tail_reconciliation_cannot_use_detached_work_as_a_branch_anchor() {
        let mut window =
            TranscriptWindow::new(vec![block("old-branch"), block("detached-bash:1")], None);
        let tail = TranscriptWindow::new(vec![block("new-branch"), block("detached-bash:1")], None);
        assert!(!window.reconcile_tail(tail.clone()));
        assert_eq!(window, tail);
    }
}
