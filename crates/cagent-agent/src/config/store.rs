use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use tokio::sync::{broadcast, watch};

use super::{
    ConfigData, ConfigSnapshot, builtin_provider_definition, config_error, ensure_child_table,
    ensure_table, implicit_table, provider_config_path,
};
use crate::RuntimeError;

const WATCH_DEBOUNCE: Duration = Duration::from_millis(200);
const GENERATED_PREFIX: &str = "# cagent-generated: generation=";

#[derive(Clone, Debug)]
pub enum ConfigChange {
    Changed {
        path: Option<PathBuf>,
        snapshot: ConfigSnapshot,
        source: ConfigChangeSource,
    },
    Rejected {
        path: PathBuf,
        message: String,
    },
}

/// Identifies whether Cagent or an external process changed the configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigChangeSource {
    /// A Cagent action persisted a configuration value.
    Internal,
    /// The watched configuration file changed outside Cagent.
    External,
}

#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<ConfigStoreInner>,
}

struct ConfigStoreInner {
    path: Option<PathBuf>,
    writer: Mutex<()>,
    snapshot: watch::Sender<ConfigSnapshot>,
    changes: broadcast::Sender<ConfigChange>,
    observed: Mutex<Option<String>>,
    self_write: Mutex<Option<(u64, String)>>,
    generation: AtomicU64,
}

fn validate_agent_name(name: &str) -> Result<(), RuntimeError> {
    if name.trim().is_empty() || name.trim() != name || name.contains('.') {
        return Err(RuntimeError::InvalidOption(
            "agent profile names must be non-empty TOML keys without whitespace or dots".into(),
        ));
    }
    Ok(())
}

fn agent_table(
    document: &mut toml_edit::DocumentMut,
) -> Result<&mut dyn toml_edit::TableLike, RuntimeError> {
    if !document.contains_key("agents") {
        document.insert("agents", toml_edit::Item::Table(implicit_table()));
    }
    document["agents"]
        .as_table_like_mut()
        .ok_or_else(|| RuntimeError::InvalidOption("agents must be a table".into()))
}

/// Rewrites only semantically known agent references, leaving arbitrary prompt
/// text and comments untouched.
fn rewrite_agent_references(table: &mut toml_edit::Table, old: &str, new: &str) {
    if table.get("default_agent").and_then(toml_edit::Item::as_str) == Some(old) {
        table["default_agent"] = toml_edit::value(new);
    }
    if let Some(agents) = table
        .get_mut("agents")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        for (_, definition) in agents.iter_mut() {
            if let Some(definition) = definition.as_table_like_mut()
                && definition.get("extends").and_then(toml_edit::Item::as_str) == Some(old)
            {
                definition.insert("extends", toml_edit::value(new));
            }
        }
    }
    rewrite_mcp_agent_assignments(table, old, new);
}

fn rewrite_mcp_agent_assignments(table: &mut toml_edit::Table, old: &str, new: &str) {
    fn visit(item: &mut toml_edit::Item, old: &str, new: &str) {
        let Some(table) = item.as_table_like_mut() else {
            return;
        };
        if let Some(agents) = table
            .get_mut("agents")
            .and_then(toml_edit::Item::as_array_mut)
        {
            for value in agents.iter_mut() {
                if value.as_str() == Some(old) {
                    *value = toml_edit::Value::from(new);
                }
            }
        }
        for (_, child) in table.iter_mut() {
            visit(child, old, new);
        }
    }
    for (_, item) in table.iter_mut() {
        visit(item, old, new);
    }
}

impl std::fmt::Debug for ConfigStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigStore")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl ConfigStore {
    /// Toggles all skills with this declared name and persists only disabled names.
    pub fn toggle_skill(&self, name: &str) -> Result<bool, RuntimeError> {
        let mut disabled = self.snapshot().disabled_skills().clone();
        let enabled = if disabled.remove(name) {
            true
        } else {
            disabled.insert(name.to_owned());
            false
        };
        let raw =
            toml::Value::Array(disabled.into_iter().map(toml::Value::String).collect()).to_string();
        self.set_value("skills.disabled", &raw)?;
        Ok(enabled)
    }
    /// Selects a ready web-search provider. The caller supplies readiness so
    /// configuration and managed credential state can be checked together.
    pub fn select_web_search_provider(
        &self,
        provider: crate::web_search::WebSearchProvider,
    ) -> Result<ConfigSnapshot, RuntimeError> {
        self.edit(move |document| {
            ensure_table(document, "web_search")?;
            let table = document["web_search"]
                .as_table_like_mut()
                .ok_or_else(|| RuntimeError::InvalidOption("web_search must be a table".into()))?;
            table.insert("provider", toml_edit::value(provider.label()));
            Ok(())
        })
    }

    pub fn clear_web_search_provider(&self) -> Result<ConfigSnapshot, RuntimeError> {
        self.edit(|document| {
            if let Some(table) = document
                .get_mut("web_search")
                .and_then(toml_edit::Item::as_table_like_mut)
            {
                table.remove("provider");
            }
            Ok(())
        })
    }

    /// Saves a SearXNG base URL and makes SearXNG active in one atomic edit.
    pub fn save_searxng_url(&self, url: &str) -> Result<ConfigSnapshot, RuntimeError> {
        let url = url.trim().to_owned();
        crate::web_search::validate_configured_url(&url)?;
        self.edit(move |document| {
            ensure_table(document, "web_search")?;
            let web_search = document["web_search"]
                .as_table_like_mut()
                .ok_or_else(|| RuntimeError::InvalidOption("web_search must be a table".into()))?;
            web_search.insert("provider", toml_edit::value("searxng"));
            ensure_child_table(web_search, "searxng")?;
            let searxng = web_search
                .get_mut("searxng")
                .and_then(toml_edit::Item::as_table_like_mut)
                .ok_or_else(|| {
                    RuntimeError::InvalidOption("web_search.searxng must be a table".into())
                })?;
            searxng.insert("url", toml_edit::value(url));
            Ok(())
        })
    }

    /// Removes only UI-managed SearXNG configuration and clears its active selection.
    pub fn remove_searxng_url(&self) -> Result<ConfigSnapshot, RuntimeError> {
        self.edit(|document| {
            let Some(web_search) = document
                .get_mut("web_search")
                .and_then(toml_edit::Item::as_table_like_mut)
            else {
                return Ok(());
            };
            if let Some(searxng) = web_search
                .get_mut("searxng")
                .and_then(toml_edit::Item::as_table_like_mut)
            {
                searxng.remove("url");
            }
            if web_search.get("provider").and_then(toml_edit::Item::as_str) == Some("searxng") {
                web_search.remove("provider");
            }
            Ok(())
        })
    }
    /// Lists the raw agent definitions in the editable document. Unlike
    /// [`ConfigSnapshot::agent_catalog`], unset fields remain unset here.
    pub fn agent_drafts(
        &self,
    ) -> Result<std::collections::BTreeMap<String, crate::AgentProfileDraft>, RuntimeError> {
        let source = self.source()?;
        let document: toml::Value = toml::from_str(&source).map_err(|error| {
            RuntimeError::InvalidOption(format!("invalid agents configuration: {error}"))
        })?;
        document
            .get("agents")
            .cloned()
            .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()))
            .try_into()
            .map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid agents configuration: {error}"))
            })
    }

    /// Atomically adds or replaces an agent definition and publishes it only
    /// after the entire resulting configuration validates.
    pub fn save_agent_draft(
        &self,
        name: &str,
        draft: &crate::AgentProfileDraft,
    ) -> Result<ConfigSnapshot, RuntimeError> {
        validate_agent_name(name)?;
        let name = name.to_owned();
        let draft = draft.clone();
        self.edit(move |document| {
            let agents = agent_table(document)?;
            let item = toml_edit::ser::to_document(&draft)
                .map_err(|error| {
                    RuntimeError::InvalidOption(format!("invalid agent draft: {error}"))
                })?
                .into_item();
            agents.insert(&name, item);
            Ok(())
        })
    }

    /// Renames a configured agent and rewrites configuration references that
    /// name it. Built-in names may be renamed by leaving a compatibility entry
    /// at the old name that extends the new profile.
    pub fn rename_agent_profile(
        &self,
        old_name: &str,
        new_name: &str,
    ) -> Result<ConfigSnapshot, RuntimeError> {
        validate_agent_name(old_name)?;
        validate_agent_name(new_name)?;
        if old_name == new_name {
            return Ok(self.snapshot());
        }
        let old_name = old_name.to_owned();
        let new_name = new_name.to_owned();
        self.edit(move |document| {
            let agents = agent_table(document)?;
            if agents.contains_key(&new_name) {
                return Err(RuntimeError::InvalidOption(format!(
                    "agent profile already exists: {new_name}"
                )));
            }
            let Some(item) = agents.remove(&old_name) else {
                return Err(RuntimeError::InvalidOption(format!(
                    "unknown configured agent profile: {old_name}"
                )));
            };
            agents.insert(&new_name, item);
            rewrite_agent_references(document.as_table_mut(), &old_name, &new_name);
            Ok(())
        })
    }
    /// Opens a file-backed configuration store and starts watching its parent directory.
    ///
    /// # Errors
    /// Returns an error when the initial candidate is unreadable or invalid.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, RuntimeError> {
        let path = path.into();
        let (source, observed) = read_source(&path)?;
        let snapshot = ConfigSnapshot::parse(&path, &source)?;
        #[cfg(test)]
        let snapshot = snapshot.disable_implicit_title_generation();
        let (snapshot_tx, _) = watch::channel(snapshot);
        let (changes, _) = broadcast::channel(64);
        let store = Self {
            inner: Arc::new(ConfigStoreInner {
                path: Some(path),
                writer: Mutex::new(()),
                snapshot: snapshot_tx,
                changes,
                observed: Mutex::new(Some(observed)),
                self_write: Mutex::new(None),
                generation: AtomicU64::new(0),
            }),
        };
        store.start_watcher();
        Ok(store)
    }

    /// Creates a store for tests and embedders without filesystem persistence.
    #[must_use]
    pub fn in_memory(snapshot: ConfigSnapshot) -> Self {
        let (snapshot, _) = watch::channel(snapshot);
        let (changes, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(ConfigStoreInner {
                path: None,
                writer: Mutex::new(()),
                snapshot,
                changes,
                observed: Mutex::new(None),
                self_write: Mutex::new(None),
                generation: AtomicU64::new(0),
            }),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> ConfigSnapshot {
        self.inner.snapshot.borrow().clone()
    }

    #[cfg(test)]
    pub(crate) fn disable_implicit_title_generation_for_tests(&self) {
        let _ = self
            .inner
            .snapshot
            .send(self.snapshot().disable_implicit_title_generation());
    }

    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<ConfigSnapshot> {
        self.inner.snapshot.subscribe()
    }

    #[must_use]
    pub fn subscribe_changes(&self) -> broadcast::Receiver<ConfigChange> {
        self.inner.changes.subscribe()
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    /// Returns the current TOML document, or the built-in empty configuration for a new file.
    pub fn source(&self) -> Result<String, RuntimeError> {
        let path = self.inner.path.as_ref().ok_or_else(|| {
            RuntimeError::InvalidOption("in-memory configuration has no source file".into())
        })?;
        let (source, _) = read_source(path)?;
        Ok(strip_generated_marker(&source).to_owned())
    }

    /// Returns a configured TOML item by dotted key.
    pub fn get_value(&self, key: &str) -> Result<Option<String>, RuntimeError> {
        let source = self.source()?;
        let document = parse_document(
            self.path().unwrap_or_else(|| Path::new("config.toml")),
            &source,
        )?;
        let mut item = None;
        let mut table: &dyn toml_edit::TableLike = document.as_table();
        for part in split_key(key)? {
            item = table.get(part);
            let Some(next) = item.and_then(toml_edit::Item::as_table_like) else {
                return Ok(item.map(|item| item.to_string().trim().to_owned()));
            };
            table = next;
        }
        Ok(item.map(|item| item.to_string().trim().to_owned()))
    }

    /// Lists dotted keys that have explicit values in the TOML document.
    pub fn keys(&self) -> Result<Vec<String>, RuntimeError> {
        let source = self.source()?;
        let document = parse_document(
            self.path().unwrap_or_else(|| Path::new("config.toml")),
            &source,
        )?;
        let mut keys = Vec::new();
        collect_keys(document.as_table(), "", &mut keys);
        Ok(keys)
    }

    /// Sets a dotted TOML key, preserving unrelated document content and validating the result.
    ///
    /// File-backed stores support this operation; in-memory stores return an explicit error because
    /// they do not retain an editable TOML document.
    pub fn set_value(&self, key: &str, raw_value: &str) -> Result<ConfigSnapshot, RuntimeError> {
        if self.inner.path.is_none() {
            return Err(RuntimeError::InvalidOption(
                "set_value requires a file-backed configuration".into(),
            ));
        }
        let parts = split_key(key)?;
        let value = parse_value(raw_value)?;
        self.edit(|document| {
            let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
            for part in &parts[..parts.len() - 1] {
                if !table.contains_key(part) {
                    table.insert(part, toml_edit::Item::Table(implicit_table()));
                }
                table = table
                    .get_mut(part)
                    .and_then(toml_edit::Item::as_table_like_mut)
                    .ok_or_else(|| RuntimeError::InvalidOption(format!("{part} is not a table")))?;
            }
            table.insert(parts[parts.len() - 1], toml_edit::Item::Value(value));
            Ok(())
        })
    }

    /// Removes only the dotted leaf override, preserving comments, parent tables, and siblings.
    ///
    /// # Errors
    /// Returns an error for an invalid dotted key, unavailable file, or invalid resulting config.
    pub fn reset_value(&self, key: &str) -> Result<ConfigSnapshot, RuntimeError> {
        if self.inner.path.is_none() {
            return Err(RuntimeError::InvalidOption(
                "reset_value requires a file-backed configuration".into(),
            ));
        }
        let parts = split_key(key)?;
        self.edit(|document| {
            remove_leaf_preserving_comments(document.as_item_mut(), &parts);
            Ok(())
        })
    }

    /// Re-reads and validates a file-backed configuration after an external editor changes it.
    pub fn reload(&self) -> Result<ConfigSnapshot, RuntimeError> {
        self.reload_from_disk()
    }

    /// Revalidates and publishes the current file after a coordinated external writer updates it.
    pub(crate) fn reload_from_disk(&self) -> Result<ConfigSnapshot, RuntimeError> {
        let _writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        let Some(path) = self.inner.path.as_ref() else {
            return Ok(self.snapshot());
        };
        let bytes = std::fs::read(path).map_err(|error| RuntimeError::Config {
            path: path.clone(),
            message: error.to_string(),
        })?;
        let (source, fingerprint) = decode_source(path, bytes)?;
        let snapshot = ConfigSnapshot::parse(path, &source)?;
        #[cfg(test)]
        let snapshot = snapshot.disable_implicit_title_generation();
        *self
            .inner
            .observed
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)? = Some(fingerprint);
        *self
            .inner
            .self_write
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)? = None;
        self.publish(snapshot.clone(), ConfigChangeSource::External);
        Ok(snapshot)
    }

    /// Persists the default model and optional effort, then publishes the validated snapshot.
    ///
    /// # Errors
    /// Returns an error for an invalid model reference or when the file cannot be updated.
    pub fn persist_model_defaults(
        &self,
        model: &str,
        effort: Option<&str>,
    ) -> Result<ConfigSnapshot, RuntimeError> {
        if self.inner.path.is_none() {
            let model = crate::ModelRef::parse(model)
                .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
            return self.update_memory(|data| {
                data.default_model = Some(super::ModelSelection {
                    target: Some(super::ModelTarget::Model(model)),
                    effort: effort.map(str::to_owned),
                });
                Ok(())
            });
        }
        self.edit(|document| {
            let mut selection = toml_edit::InlineTable::new();
            selection.insert("model", toml_edit::Value::from(model));
            if let Some(effort) = effort {
                selection.insert("effort", toml_edit::Value::from(effort));
            }
            document["default_model"] = toml_edit::value(selection);
            Ok(())
        })
    }

    /// Persists the complete favourite-model set and publishes the validated snapshot.
    ///
    /// # Errors
    /// Model availability is resolved later, so references to providers that are not
    /// currently configured are retained.
    pub fn persist_model_favourites(
        &self,
        favourites: &BTreeSet<crate::ModelRef>,
    ) -> Result<ConfigSnapshot, RuntimeError> {
        if self.inner.path.is_none() {
            let favourites = favourites.clone();
            return self.update_memory(|data| {
                data.favourite_models = favourites;
                Ok(())
            });
        }
        self.edit(|document| {
            if favourites.is_empty() {
                document.remove("favourite_models");
            } else {
                let mut values = toml_edit::Array::new();
                for favourite in favourites {
                    values.push(favourite.to_string());
                }
                document["favourite_models"] = toml_edit::value(values);
            }
            Ok(())
        })
    }

    /// Persists the global Fast service-tier preference and publishes it immediately.
    pub fn persist_fast(&self, enabled: bool) -> Result<ConfigSnapshot, RuntimeError> {
        if self.inner.path.is_none() {
            return self.update_memory(|data| {
                data.fast = enabled;
                Ok(())
            });
        }
        self.edit(|document| {
            document["fast"] = toml_edit::value(enabled);
            Ok(())
        })
    }

    /// Persists status-line layout and colors and publishes the validated snapshot.
    ///
    /// # Errors
    /// Returns an error for an invalid layout or when the file cannot be updated.
    pub fn persist_status_line(
        &self,
        status_line: &crate::StatusLineConfig,
    ) -> Result<ConfigSnapshot, RuntimeError> {
        let unique = status_line.modules.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != status_line.modules.len() {
            return Err(RuntimeError::InvalidOption(
                "statusline modules must not contain duplicates".into(),
            ));
        }
        if self.inner.path.is_none() {
            let status_line = status_line.clone();
            return self.update_memory(|data| {
                data.status_line = status_line;
                Ok(())
            });
        }
        self.edit(|document| {
            ensure_table(document, "ui")?;
            let ui = document
                .get_mut("ui")
                .and_then(toml_edit::Item::as_table_like_mut)
                .ok_or_else(|| RuntimeError::InvalidOption("ui must be a table".into()))?;
            ensure_child_table(ui, "statusline")?;
            let table = ui
                .get_mut("statusline")
                .and_then(toml_edit::Item::as_table_like_mut)
                .ok_or_else(|| {
                    RuntimeError::InvalidOption("ui.statusline must be a table".into())
                })?;
            let mut modules = toml_edit::Array::new();
            for module in &status_line.modules {
                modules.push(module.to_string());
            }
            table.insert("modules", toml_edit::value(modules));
            let overrides = crate::StatusLineModule::ALL
                .into_iter()
                .filter(|module| *module != crate::StatusLineModule::Mode)
                .filter_map(|module| {
                    let color = status_line.color(module);
                    (color != module.default_color()).then_some((module, color))
                })
                .collect::<Vec<_>>();
            if overrides.is_empty() {
                table.remove("colors");
            } else {
                let mut colors = implicit_table();
                for (module, color) in overrides {
                    colors.insert(&module.to_string(), toml_edit::value(color.to_string()));
                }
                table.insert("colors", toml_edit::Item::Table(colors));
            }
            Ok(())
        })
    }

    /// Persists explicit provider enablement and publishes the validated snapshot.
    ///
    /// # Errors
    /// Returns an error for an unknown provider or when the file cannot be updated.
    pub fn persist_provider_enabled(
        &self,
        provider_id: &str,
        enabled: bool,
    ) -> Result<ConfigSnapshot, RuntimeError> {
        let current = self.snapshot();
        let settings = current.provider(provider_id).cloned().ok_or_else(|| {
            RuntimeError::InvalidOption(format!("provider is not configured: {provider_id}"))
        })?;
        if self.inner.path.is_none() {
            let provider_id = provider_id.to_owned();
            return self.update_memory(|data| {
                data.providers
                    .get_mut(&provider_id)
                    .ok_or_else(|| {
                        RuntimeError::InvalidOption(format!(
                            "provider is not configured: {provider_id}"
                        ))
                    })?
                    .enabled = enabled;
                Ok(())
            });
        }
        self.edit(|document| {
            if !document.contains_key("providers") {
                document.insert("providers", toml_edit::Item::Table(implicit_table()));
            }
            let providers = document
                .get_mut("providers")
                .and_then(toml_edit::Item::as_table_like_mut)
                .ok_or_else(|| RuntimeError::InvalidOption("providers must be a table".into()))?;
            let parent = if builtin_provider_definition(provider_id).is_some() {
                providers
            } else {
                if !providers.contains_key("custom") {
                    providers.insert("custom", toml_edit::Item::Table(implicit_table()));
                }
                providers
                    .get_mut("custom")
                    .and_then(toml_edit::Item::as_table_like_mut)
                    .ok_or_else(|| {
                        RuntimeError::InvalidOption("providers.custom must be a table".into())
                    })?
            };
            if !parent.contains_key(provider_id) {
                parent.insert(provider_id, toml_edit::Item::Table(implicit_table()));
            }
            let provider = parent
                .get_mut(provider_id)
                .and_then(toml_edit::Item::as_table_like_mut)
                .ok_or_else(|| {
                    RuntimeError::InvalidOption(format!(
                        "{} must be a table",
                        provider_config_path(provider_id)
                    ))
                })?;
            if builtin_provider_definition(provider_id).is_some() {
                provider.remove("type");
            } else if !provider.contains_key("type") {
                provider.insert("type", toml_edit::value(settings.kind.to_string()));
            }
            provider.insert("enabled", toml_edit::value(enabled));
            Ok(())
        })
    }

    fn update_memory(
        &self,
        update: impl FnOnce(&mut ConfigData) -> Result<(), RuntimeError>,
    ) -> Result<ConfigSnapshot, RuntimeError> {
        let _writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        let mut data = (*self.snapshot().0).clone();
        update(&mut data)?;
        let snapshot = ConfigSnapshot(Arc::new(data));
        self.publish(snapshot.clone(), ConfigChangeSource::Internal);
        Ok(snapshot)
    }

    fn edit(
        &self,
        edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), RuntimeError>,
    ) -> Result<ConfigSnapshot, RuntimeError> {
        let _writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        let path = self.inner.path.as_ref().expect("file-backed edit");
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|error| RuntimeError::Config {
            path: path.clone(),
            message: error.to_string(),
        })?;
        let mut document = read_document(path)?;
        if !document.contains_key("version") {
            document.insert(
                "version",
                toml_edit::value(i64::from(super::CONFIG_VERSION)),
            );
        }
        edit(&mut document)?;
        let body = document.to_string();
        let snapshot = ConfigSnapshot::parse(path, &body)?;
        #[cfg(test)]
        let snapshot = snapshot.disable_implicit_title_generation();
        let generation = self.inner.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let body_hash = fingerprint_bytes(body.as_bytes());
        let contents = format!("{GENERATED_PREFIX}{generation} sha256={body_hash}\n{body}");
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|error| RuntimeError::Config {
                path: path.clone(),
                message: error.to_string(),
            })?;
        temporary
            .write_all(contents.as_bytes())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| RuntimeError::Config {
                path: path.clone(),
                message: error.to_string(),
            })?;
        temporary
            .persist(path)
            .map_err(|error| RuntimeError::Config {
                path: path.clone(),
                message: error.error.to_string(),
            })?;
        *self
            .inner
            .self_write
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)? = Some((generation, body_hash));
        *self
            .inner
            .observed
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)? =
            Some(fingerprint_bytes(contents.as_bytes()));
        self.publish(snapshot.clone(), ConfigChangeSource::Internal);
        Ok(snapshot)
    }

    fn publish(&self, snapshot: ConfigSnapshot, source: ConfigChangeSource) {
        self.inner.snapshot.send_replace(snapshot.clone());
        let _ = self.inner.changes.send(ConfigChange::Changed {
            path: self.inner.path.clone(),
            snapshot,
            source,
        });
    }

    fn start_watcher(&self) {
        let weak = Arc::downgrade(&self.inner);
        std::thread::Builder::new()
            .name("cagent-config-watch".into())
            .spawn(move || watch_loop(&weak))
            .expect("configuration watcher thread must start");
    }
}

fn remove_leaf_preserving_comments(item: &mut toml_edit::Item, parts: &[&str]) {
    let Some((part, rest)) = parts.split_first() else {
        return;
    };
    if !rest.is_empty() {
        let Some(child) = item
            .as_table_like_mut()
            .and_then(|table| table.get_mut(part))
        else {
            return;
        };
        remove_leaf_preserving_comments(child, rest);
        return;
    }

    let (retained, previous, next) = {
        let Some(table) = item.as_table_like() else {
            return;
        };
        let keys = table
            .iter()
            .map(|(key, _)| key.to_owned())
            .collect::<Vec<_>>();
        let Some(index) = keys.iter().position(|key| key == part) else {
            return;
        };
        let key_prefix = table
            .key(part)
            .and_then(|key| key.leaf_decor().prefix())
            .and_then(toml_edit::RawString::as_str)
            .unwrap_or_default();
        let value_suffix = table
            .get(part)
            .and_then(toml_edit::Item::as_value)
            .and_then(|value| value.decor().suffix())
            .and_then(toml_edit::RawString::as_str)
            .unwrap_or_default();
        (
            format!("{key_prefix}{value_suffix}"),
            index
                .checked_sub(1)
                .and_then(|index| keys.get(index))
                .cloned(),
            keys.get(index + 1).cloned(),
        )
    };

    let Some(table) = item.as_table_like_mut() else {
        return;
    };
    table.remove(part);
    if retained.trim().is_empty() {
        return;
    }
    if let Some(next) = next {
        let existing = table
            .key(&next)
            .and_then(|key| key.leaf_decor().prefix())
            .and_then(toml_edit::RawString::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(mut key) = table.key_mut(&next) {
            key.leaf_decor_mut()
                .set_prefix(format!("{retained}{existing}"));
        }
    } else if let Some(previous) = previous
        && let Some(value) = table
            .get_mut(&previous)
            .and_then(toml_edit::Item::as_value_mut)
    {
        let existing = value
            .decor()
            .suffix()
            .and_then(toml_edit::RawString::as_str)
            .unwrap_or_default()
            .to_owned();
        value
            .decor_mut()
            .set_suffix(format!("{existing}{retained}"));
    } else if let Some(table) = item.as_table_mut() {
        let existing = table
            .decor()
            .suffix()
            .and_then(toml_edit::RawString::as_str)
            .unwrap_or_default()
            .to_owned();
        table
            .decor_mut()
            .set_suffix(format!("{existing}{retained}"));
    }
}

impl From<ConfigSnapshot> for ConfigStore {
    fn from(snapshot: ConfigSnapshot) -> Self {
        Self::in_memory(snapshot)
    }
}

fn watch_loop(weak: &Weak<ConfigStoreInner>) {
    loop {
        std::thread::sleep(WATCH_DEBOUNCE);
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let Some(path) = inner.path.as_ref() else {
            return;
        };
        let (source, fingerprint) = match std::fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes.clone()) {
                Ok(source) => (Some(source), fingerprint_bytes(&bytes)),
                Err(error) => {
                    reject_once(&inner, path, fingerprint_bytes(&bytes), error.to_string());
                    continue;
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, "missing".into()),
            Err(error) => {
                reject_once(
                    &inner,
                    path,
                    format!("io:{:?}:{error}", error.kind()),
                    error.to_string(),
                );
                continue;
            }
        };
        let unchanged = inner
            .observed
            .lock()
            .is_ok_and(|observed| observed.as_deref() == Some(&fingerprint));
        if unchanged {
            continue;
        }
        if let Some(source) = source.as_deref()
            && let Some((generation, expected, body)) = generated_marker(source)
            && fingerprint_bytes(body.as_bytes()) == expected
            && inner
                .self_write
                .lock()
                .is_ok_and(|write| write.as_ref() == Some(&(generation, expected.clone())))
        {
            if let Ok(mut observed) = inner.observed.lock() {
                *observed = Some(fingerprint);
            }
            continue;
        }
        if let Ok(mut observed) = inner.observed.lock() {
            *observed = Some(fingerprint);
        }
        let candidate = source.unwrap_or_else(|| "version = 1\n".into());
        match ConfigSnapshot::parse(path, &candidate) {
            Ok(snapshot) => {
                #[cfg(test)]
                let snapshot = snapshot.disable_implicit_title_generation();
                inner.snapshot.send_replace(snapshot.clone());
                let _ = inner.changes.send(ConfigChange::Changed {
                    path: Some(path.clone()),
                    snapshot,
                    source: ConfigChangeSource::External,
                });
            }
            Err(error) => {
                let _ = inner.changes.send(ConfigChange::Rejected {
                    path: path.clone(),
                    message: error.to_string(),
                });
            }
        }
    }
}

fn reject_once(inner: &ConfigStoreInner, path: &Path, fingerprint: String, message: String) {
    let repeated = inner
        .observed
        .lock()
        .is_ok_and(|observed| observed.as_deref() == Some(&fingerprint));
    if repeated {
        return;
    }
    if let Ok(mut observed) = inner.observed.lock() {
        *observed = Some(fingerprint);
    }
    let _ = inner.changes.send(ConfigChange::Rejected {
        path: path.to_path_buf(),
        message,
    });
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_source(path: &Path) -> Result<(String, String), RuntimeError> {
    match std::fs::read(path) {
        Ok(bytes) => decode_source(path, bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(("version = 1\n".into(), "missing".into()))
        }
        Err(error) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

fn decode_source(path: &Path, bytes: Vec<u8>) -> Result<(String, String), RuntimeError> {
    let source = String::from_utf8(bytes.clone()).map_err(|error| config_error(path, error))?;
    Ok((source, fingerprint_bytes(&bytes)))
}

fn read_document(path: &Path) -> Result<toml_edit::DocumentMut, RuntimeError> {
    let (source, _) = read_source(path)?;
    parse_document(path, &source)
}

fn parse_document(path: &Path, source: &str) -> Result<toml_edit::DocumentMut, RuntimeError> {
    strip_generated_marker(source)
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| config_error(path, error))
}

fn strip_generated_marker(source: &str) -> &str {
    generated_marker(source).map_or(source, |(_, _, body)| body)
}

fn split_key(key: &str) -> Result<Vec<&str>, RuntimeError> {
    let parts = key.split('.').collect::<Vec<_>>();
    if parts.is_empty() || parts.iter().any(|part| part.trim().is_empty()) {
        return Err(RuntimeError::InvalidOption(
            "config key must contain non-empty dotted components".into(),
        ));
    }
    Ok(parts)
}

fn parse_value(raw_value: &str) -> Result<toml_edit::Value, RuntimeError> {
    let document = format!("value = {raw_value}\n")
        .parse::<toml_edit::DocumentMut>()
        .or_else(|_| format!("value = {:?}\n", raw_value).parse::<toml_edit::DocumentMut>())
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid TOML value: {error}")))?;
    document
        .get("value")
        .and_then(toml_edit::Item::as_value)
        .cloned()
        .ok_or_else(|| RuntimeError::InvalidOption("config value must be a TOML value".into()))
}

fn collect_keys(table: &dyn toml_edit::TableLike, prefix: &str, keys: &mut Vec<String>) {
    for (name, item) in table.iter() {
        let key = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}.{name}")
        };
        if let Some(table) = item.as_table_like() {
            collect_keys(table, &key, keys);
        } else if !item.is_none() {
            keys.push(key);
        }
    }
}

fn generated_marker(source: &str) -> Option<(u64, String, &str)> {
    let (line, body) = source.split_once('\n')?;
    let values = line.strip_prefix(GENERATED_PREFIX)?;
    let (generation, hash) = values.split_once(" sha256=")?;
    Some((generation.parse().ok()?, hash.to_owned(), body))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn self_write_preserves_comments_and_publishes_once() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "# retained\nversion = 1\n").unwrap();
        let store = ConfigStore::open(&path).unwrap();
        let mut changes = store.subscribe_changes();

        store
            .persist_model_defaults("openai/gpt-test", Some("high"))
            .unwrap();
        assert!(matches!(
            changes.recv().await.unwrap(),
            ConfigChange::Changed {
                source: ConfigChangeSource::Internal,
                ..
            }
        ));
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert!(matches!(
            changes.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let persisted = std::fs::read_to_string(path).unwrap();
        assert!(persisted.contains("# retained"));
        assert_eq!(persisted.matches(GENERATED_PREFIX).count(), 1);
    }

    #[test]
    fn fast_persists_without_removing_comments() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "# retained\nversion = 1\n").unwrap();
        let store = ConfigStore::open(&path).unwrap();
        let snapshot = store.persist_fast(true).unwrap();
        assert!(snapshot.fast());
        let persisted = std::fs::read_to_string(path).unwrap();
        assert!(persisted.contains("# retained"));
        assert!(persisted.contains("fast = true"));
    }

    #[test]
    fn in_memory_fast_publishes_immediately() {
        let store = ConfigStore::in_memory(ConfigSnapshot::default());
        assert!(!store.snapshot().fast());
        assert!(store.persist_fast(true).unwrap().fast());
        assert!(store.snapshot().fast());
    }

    #[test]
    fn generic_config_values_preserve_comments_and_validate() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            "# retained\nversion = 1\n[providers.mock]\nenabled = true\n",
        )
        .unwrap();
        let store = ConfigStore::open(&path).unwrap();

        store
            .set_value("default_model", r#"{ model = "mock/echo" }"#)
            .unwrap();

        assert_eq!(
            store.get_value("default_model").unwrap().as_deref(),
            Some(r#"{ model = "mock/echo" }"#)
        );
        assert!(
            store
                .keys()
                .unwrap()
                .contains(&"default_model.model".to_owned())
        );
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("# retained")
        );
    }

    #[test]
    fn in_memory_model_preferences_allow_unconfigured_providers() {
        let store = ConfigStore::in_memory(ConfigSnapshot::default());
        let model = "google/gemini-future";
        store.persist_model_defaults(model, None).unwrap();
        store
            .persist_model_favourites(&BTreeSet::from([crate::ModelRef::parse(model).unwrap()]))
            .unwrap();

        assert_eq!(store.snapshot().default_model().unwrap().to_string(), model);
        assert!(
            store
                .snapshot()
                .favourite_models()
                .contains(&crate::ModelRef::parse(model).unwrap())
        );
    }

    #[test]
    fn reset_value_removes_only_the_leaf_and_preserves_comments() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            "# retained\nversion = 1\n[ui]\n# rows comment\ncomposer_max_rows = 8\ndiff_context_lines = 5\n",
        )
        .unwrap();
        let store = ConfigStore::open(&path).unwrap();

        store.reset_value("ui.composer_max_rows").unwrap();

        let source = store.source().unwrap();
        assert!(source.contains("# retained"));
        assert!(source.contains("# rows comment"));
        assert!(!source.contains("composer_max_rows"));
        assert!(source.contains("diff_context_lines = 5"));
        assert_eq!(store.snapshot().composer_max_rows(), None);
    }

    #[test]
    fn first_file_backed_edit_seeds_version_in_an_empty_config() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let store = ConfigStore::open(&path).unwrap();

        store.persist_provider_enabled("mock", true).unwrap();

        let source = store.source().unwrap();
        assert!(source.contains("version = 1"));
        assert!(source.contains("[providers.mock]"));
        assert!(store.snapshot().provider_enabled("mock"));
    }

    #[test]
    fn in_memory_set_value_returns_a_configuration_error() {
        let store = ConfigStore::in_memory(ConfigSnapshot::default());

        let error = store.set_value("ui.composer_max_rows", "8").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("set_value requires a file-backed configuration")
        );
    }

    #[test]
    fn set_value_hides_implicit_parent_tables() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "# keep me\nversion = 1\n").unwrap();
        let store = ConfigStore::open(&path).unwrap();

        store.set_value("a.b.c.x", "true").unwrap();

        let source = store.source().unwrap();
        assert!(source.contains("[a.b.c]"));
        assert!(source.contains("x = true"));
        assert!(!source.contains("\n[a]\n"));
        assert!(!source.contains("\n[a.b]\n"));
        assert!(source.contains("# keep me"));
    }

    #[test]
    fn generated_ui_containers_hide_redundant_parent_headers() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(&path).unwrap();

        store
            .persist_status_line(&crate::StatusLineConfig::default())
            .unwrap();

        let source = store.source().unwrap();
        assert!(source.contains("[ui.statusline]"));
        assert!(!source.contains("\n[ui]\n"));
        assert!(!source.contains("\n[ui.statusline.colors]\n"));
    }

    #[tokio::test]
    async fn watches_atomic_creation_edit_and_deletion() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        let store = ConfigStore::open(&path).unwrap();
        let mut snapshots = store.subscribe();
        let replacement = temporary.path().join("replacement.toml");
        std::fs::write(
            &replacement,
            "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\nenabled = true\n",
        )
        .unwrap();
        std::fs::rename(replacement, &path).unwrap();

        tokio::time::timeout(Duration::from_secs(2), snapshots.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshots
                .borrow_and_update()
                .default_model()
                .map(ToString::to_string),
            Some("mock/echo".into())
        );

        std::fs::remove_file(&path).unwrap();
        tokio::time::timeout(Duration::from_secs(2), snapshots.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(snapshots.borrow_and_update().default_model().is_none());
    }

    #[tokio::test]
    async fn invalid_edit_retains_last_snapshot_and_reports_once() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(&path).unwrap();
        let mut changes = store.subscribe_changes();
        let original = store.snapshot();

        std::fs::write(&path, "version = [\n").unwrap();
        let change = tokio::time::timeout(Duration::from_secs(2), changes.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(change, ConfigChange::Rejected { .. }));
        assert_eq!(store.snapshot().default_mode(), original.default_mode());
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert!(matches!(
            changes.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn invalid_mcp_edit_retains_last_snapshot() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            "version = 1\n[mcp.servers.docs]\ntransport = 'stdio'\ncommand = 'docs'\n",
        )
        .unwrap();
        let store = ConfigStore::open(&path).unwrap();
        let mut changes = store.subscribe_changes();
        let original = store.snapshot();

        std::fs::write(
            &path,
            "version = 1\n[mcp.servers.docs]\ntransport = 'stdio'\n",
        )
        .unwrap();
        let change = tokio::time::timeout(Duration::from_secs(2), changes.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(change, ConfigChange::Rejected { .. }));
        assert!(Arc::ptr_eq(&original.0, &store.snapshot().0));
    }

    #[test]
    fn agent_drafts_preserve_unset_values_and_validate_atomically() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep\nversion = 1\n[agents.child]\nextends = 'general'\n",
        )
        .unwrap();
        let store = ConfigStore::open(&path).unwrap();
        let mut drafts = store.agent_drafts().unwrap();
        let draft = drafts.remove("child").unwrap();
        assert_eq!(draft.extends.as_deref(), Some("general"));
        assert_eq!(draft.prompt, None);

        let invalid = crate::AgentProfileDraft {
            extends: Some("missing".into()),
            ..Default::default()
        };
        assert!(store.save_agent_draft("broken", &invalid).is_err());
        assert!(!store.source().unwrap().contains("[agents.broken]"));

        let valid = crate::AgentProfileDraft {
            extends: Some("general".into()),
            prompt: Some("Review carefully.".into()),
            prompt_merge: Some(crate::PromptMerge::Append),
            availability: Some(crate::AgentAvailability::Subagent),
            ..Default::default()
        };
        store.save_agent_draft("review", &valid).unwrap();
        assert_eq!(
            store
                .snapshot()
                .agent_catalog()
                .unwrap()
                .get("review")
                .unwrap()
                .availability,
            crate::AgentAvailability::Subagent
        );
        assert!(store.source().unwrap().contains("# keep"));
    }
}
