//! Asynchronous, reusable workspace indexes for frontend path completion.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::RuntimeError;
use crate::runtime::workspace_support::WorkspaceSupport as ReadOnlyTools;

fn fuzzy_path_score(path: &std::path::Path, query: &str) -> Option<usize> {
    let candidate = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let query = query.replace('\\', "/").to_lowercase();
    if query.is_empty() {
        return Some(candidate.matches('/').count() * 8);
    }

    let basename = candidate.rsplit('/').next().unwrap_or(&candidate);
    if basename.starts_with(&query) {
        return Some(basename.len() - query.len());
    }
    if candidate.starts_with(&query) {
        return Some(20 + candidate.len() - query.len());
    }
    if let Some(offset) = basename.find(&query) {
        return Some(40 + offset * 2 + basename.len() - query.len());
    }
    if let Some(offset) = candidate.find(&query) {
        return Some(80 + offset + candidate.len() - query.len());
    }

    let mut matched = 0;
    let mut first = None;
    let mut previous = None;
    let mut gaps = 0_usize;
    let query_chars = query.chars().collect::<Vec<_>>();
    for (index, character) in candidate.char_indices() {
        if query_chars.get(matched) != Some(&character) {
            continue;
        }
        first.get_or_insert(index);
        if let Some(previous) = previous {
            gaps = gaps.saturating_add(index.saturating_sub(previous + 1));
        }
        previous = Some(index);
        matched += 1;
        if matched == query_chars.len() {
            return Some(160 + first.unwrap_or_default() + gaps * 3);
        }
    }
    None
}

#[tracing::instrument(level = "trace", name = "agent.path_completion.index", skip_all)]
async fn build_path_index(
    tools: &ReadOnlyTools,
    cancellation: CancellationToken,
) -> Result<Vec<crate::WorkspaceEntry>, RuntimeError> {
    let started_at = std::time::Instant::now();
    let recursive = tools
        .workspace_path_candidates(&cancellation)
        .await
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
    let recursive_candidate_count = recursive.len();
    let mut candidates = HashMap::new();
    for entry in recursive {
        for ancestor in entry.path.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            candidates
                .entry(ancestor.to_path_buf())
                .or_insert(crate::WorkspaceEntryKind::Directory);
        }
        candidates.insert(entry.path, entry.kind);
    }
    let mut entries = candidates
        .into_iter()
        .map(|(path, kind)| crate::WorkspaceEntry { path, kind })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    tracing::trace!(
        elapsed = ?started_at.elapsed(),
        recursive_candidate_count,
        indexed_candidate_count = entries.len(),
        "built workspace path completion index"
    );
    Ok(entries)
}

/// A reusable workspace corpus for one active frontend path-completion token.
///
/// Construction is cheap. The first completion request starts the scan, and
/// later query edits rerank the same index. Starting a new completion session
/// performs a fresh scan so newly created paths can appear.
#[derive(Clone)]
pub struct PathCompletionSession {
    tools: ReadOnlyTools,
    config: crate::ConfigStore,
    options: (bool, bool),
    lifetime: Arc<PathCompletionLifetime>,
    index: Arc<PathCompletionIndex>,
}

struct PathCompletionLifetime {
    cancellation: CancellationToken,
}

struct PathCompletionIndex {
    result: tokio::sync::OnceCell<Result<Vec<crate::WorkspaceEntry>, String>>,
}

impl Drop for PathCompletionLifetime {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl PathCompletionSession {
    pub(super) fn new(tools: ReadOnlyTools, config: crate::ConfigStore) -> Self {
        let snapshot = config.snapshot();
        let options = (
            snapshot.file_picker_respect_gitignore(),
            snapshot.file_picker_hide_hidden_files(),
        );
        let lifetime = Arc::new(PathCompletionLifetime {
            cancellation: CancellationToken::new(),
        });
        let index = Arc::new(PathCompletionIndex {
            result: tokio::sync::OnceCell::new(),
        });
        Self {
            tools,
            config,
            options,
            lifetime,
            index,
        }
    }

    fn current_options(&self) -> (bool, bool) {
        let snapshot = self.config.snapshot();
        (
            snapshot.file_picker_respect_gitignore(),
            snapshot.file_picker_hide_hidden_files(),
        )
    }

    fn configured_tools(&self, options: (bool, bool)) -> ReadOnlyTools {
        self.tools
            .clone()
            .with_file_picker_options(options.0, options.1)
    }

    async fn candidates(&self) -> Result<Vec<crate::WorkspaceEntry>, RuntimeError> {
        let current = self.current_options();
        if current != self.options {
            return build_path_index(
                &self.configured_tools(current),
                self.lifetime.cancellation.clone(),
            )
            .await;
        }
        self.index
            .result
            .get_or_init(|| async {
                build_path_index(
                    &self.configured_tools(self.options),
                    self.lifetime.cancellation.clone(),
                )
                .await
                .map_err(|error| error.to_string())
            })
            .await
            .as_ref()
            .cloned()
            .map_err(|message| {
                RuntimeError::InvalidOption(format!("workspace path index failed: {message}"))
            })
    }

    /// Fuzzily completes paths from an already-built corpus without starting
    /// a workspace scan.
    #[must_use]
    pub fn complete_cached(
        &self,
        query: &str,
    ) -> Option<Result<Vec<crate::WorkspaceEntry>, RuntimeError>> {
        if is_external_query(query) {
            return None;
        }
        if self.current_options() != self.options {
            return None;
        }
        let result = self.index.result.get()?;
        Some(
            result
                .as_ref()
                .map(|candidates| rank_path_candidates(candidates, query))
                .map_err(|message| {
                    RuntimeError::InvalidOption(format!("workspace path index failed: {message}"))
                }),
        )
    }

    /// Builds the workspace corpus without producing completion rows.
    ///
    /// This is useful for frontends that can show a lightweight direct
    /// directory listing while preparing the reusable recursive index.
    ///
    /// # Errors
    ///
    /// Returns a structured runtime error when the workspace cannot be scanned.
    pub async fn warm(&self) -> Result<(), RuntimeError> {
        self.candidates().await.map(|_| ())
    }

    /// Fuzzily completes paths from this session's corpus.
    ///
    /// Workspace-relative queries use the reusable recursive index. Home and
    /// absolute queries list and rank only the direct entries in the directory
    /// implied by the query.
    ///
    /// # Errors
    ///
    /// Returns a structured runtime error when the workspace cannot be scanned.
    pub async fn complete(&self, query: &str) -> Result<Vec<crate::WorkspaceEntry>, RuntimeError> {
        if is_external_query(query) {
            return complete_external_path(&self.configured_tools(self.current_options()), query);
        }
        Ok(rank_path_candidates(&self.candidates().await?, query))
    }
}

fn is_external_query(query: &str) -> bool {
    query.starts_with('~') || std::path::Path::new(query).is_absolute()
}

fn complete_external_path(
    tools: &ReadOnlyTools,
    query: &str,
) -> Result<Vec<crate::WorkspaceEntry>, RuntimeError> {
    let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    complete_external_path_with_home(tools, query, home.as_deref())
}

fn complete_external_path_with_home(
    tools: &ReadOnlyTools,
    query: &str,
    home: Option<&std::path::Path>,
) -> Result<Vec<crate::WorkspaceEntry>, RuntimeError> {
    let (root, display_root, remainder) = if query == "~" {
        let home = home
            .ok_or_else(|| RuntimeError::InvalidOption("home directory is unavailable".into()))?;
        (home.to_path_buf(), std::path::PathBuf::from("~"), "")
    } else if let Some(remainder) = query.strip_prefix("~/") {
        let home = home
            .ok_or_else(|| RuntimeError::InvalidOption("home directory is unavailable".into()))?;
        (home.to_path_buf(), std::path::PathBuf::from("~"), remainder)
    } else if query.starts_with('~') {
        return Err(RuntimeError::InvalidOption(
            "named-user home paths are unsupported; use ~ or ~/path".into(),
        ));
    } else {
        let path = std::path::Path::new(query);
        if !path.is_absolute() {
            return Ok(Vec::new());
        }
        let remainder = query.trim_start_matches('/');
        (
            std::path::PathBuf::from("/"),
            std::path::PathBuf::from("/"),
            remainder,
        )
    };

    let (directory, basename) = remainder
        .rsplit_once('/')
        .map_or(("", remainder), |(directory, basename)| {
            (directory, basename)
        });
    let filesystem_directory = root.join(directory);
    let display_directory = display_root.join(directory);
    let entries = tools
        .visible_directory(&filesystem_directory)
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
        .into_iter()
        .filter_map(|entry| {
            let name = entry.path.file_name()?;
            Some(crate::WorkspaceEntry {
                path: display_directory.join(name),
                kind: entry.kind,
            })
        })
        .collect::<Vec<_>>();
    Ok(rank_direct_path_candidates(&entries, basename))
}

fn rank_direct_path_candidates(
    candidates: &[crate::WorkspaceEntry],
    query: &str,
) -> Vec<crate::WorkspaceEntry> {
    const RESULT_LIMIT: usize = 100;
    let mut ranked = candidates
        .iter()
        .filter_map(|entry| {
            let name = entry.path.file_name()?;
            fuzzy_path_score(std::path::Path::new(name), query).map(|score| {
                (
                    usize::from(entry.kind == crate::WorkspaceEntryKind::Directory),
                    score,
                    entry.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        (left.0, left.1, &left.2.path).cmp(&(right.0, right.1, &right.2.path))
    });
    ranked
        .into_iter()
        .take(RESULT_LIMIT)
        .map(|(_, _, entry)| entry)
        .collect()
}

fn rank_path_candidates(
    candidates: &[crate::WorkspaceEntry],
    query: &str,
) -> Vec<crate::WorkspaceEntry> {
    const RESULT_LIMIT: usize = 100;
    let mut ranked = candidates
        .iter()
        .filter_map(|entry| {
            fuzzy_path_score(&entry.path, query).map(|score| {
                (
                    usize::from(entry.kind == crate::WorkspaceEntryKind::Directory),
                    score,
                    entry.path.components().count(),
                    entry.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        (left.0, left.1, left.2, &left.3.path).cmp(&(right.0, right.1, right.2, &right.3.path))
    });
    ranked
        .into_iter()
        .take(RESULT_LIMIT)
        .map(|(_, _, _, entry)| entry)
        .collect()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn existing_indexes_are_invalidated_when_picker_options_change() {
        let temporary = TempDir::new().unwrap();
        std::fs::create_dir(temporary.path().join(".git")).unwrap();
        std::fs::write(temporary.path().join(".hidden.txt"), "").unwrap();
        std::fs::write(temporary.path().join("visible.txt"), "").unwrap();
        let config_path = temporary.path().join("config.toml");
        std::fs::write(&config_path, "version = 1\n").unwrap();
        let config = crate::ConfigStore::open(config_path).unwrap();
        let completion = PathCompletionSession::new(
            ReadOnlyTools::new(temporary.path()).unwrap(),
            config.clone(),
        );

        let initial = completion.complete("").await.unwrap();
        assert!(
            !initial
                .iter()
                .any(|entry| entry.path == std::path::Path::new(".hidden.txt"))
        );

        config
            .save_setting("ui.file_picker.hide_hidden_files", "false")
            .unwrap();
        assert!(completion.complete_cached("").is_none());
        let refreshed = completion.complete("").await.unwrap();
        assert!(
            refreshed
                .iter()
                .any(|entry| entry.path == std::path::Path::new(".hidden.txt"))
        );
    }

    #[test]
    fn external_completion_lists_one_directory_and_preserves_typed_roots() {
        let temporary = TempDir::new().unwrap();
        let home = temporary.path().join("home");
        std::fs::create_dir_all(home.join("Documents/nested")).unwrap();
        std::fs::write(home.join("Documents/notes.txt"), "").unwrap();
        std::fs::write(home.join("Documents/nested/deep.txt"), "").unwrap();
        std::fs::write(home.join("Downloads.txt"), "").unwrap();
        std::fs::write(home.join(".hidden.txt"), "").unwrap();
        let tools = ReadOnlyTools::new(temporary.path()).unwrap();

        let home_rows = complete_external_path_with_home(&tools, "~/Do", Some(&home)).unwrap();
        assert!(
            home_rows
                .iter()
                .any(|entry| entry.path == std::path::Path::new("~/Documents"))
        );
        assert!(
            home_rows
                .iter()
                .any(|entry| entry.path == std::path::Path::new("~/Downloads.txt"))
        );
        assert!(
            home_rows
                .iter()
                .all(|entry| entry.path != std::path::Path::new("~/Documents/notes.txt"))
        );
        assert!(
            home_rows
                .iter()
                .all(|entry| entry.path != std::path::Path::new("~/.hidden.txt"))
        );

        let nested =
            complete_external_path_with_home(&tools, "~/Documents/no", Some(&home)).unwrap();
        assert_eq!(
            nested[0].path,
            std::path::Path::new("~/Documents/notes.txt")
        );

        let absolute_query = format!("{}/Documents/no", home.to_string_lossy());
        let absolute =
            complete_external_path_with_home(&tools, &absolute_query, Some(&home)).unwrap();
        assert_eq!(absolute[0].path, home.join("Documents/notes.txt"));
    }

    #[test]
    fn named_user_home_completion_is_rejected() {
        let temporary = TempDir::new().unwrap();
        let tools = ReadOnlyTools::new(temporary.path()).unwrap();
        let error =
            complete_external_path_with_home(&tools, "~someone/file", Some(temporary.path()))
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("named-user home paths are unsupported")
        );
    }
}
