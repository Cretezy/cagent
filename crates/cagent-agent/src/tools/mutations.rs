//! Deterministic, frontend-neutral planning and execution for text mutations.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const EDIT_INPUT_LIMIT: u64 = 2 * 1024 * 1024;
const EDIT_OUTPUT_LIMIT: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyPatchRequest {
    pub patch: String,
}

/// Splits a valid multi-file patch into provider-neutral single-file requests.
pub(crate) fn split_apply_patch_request(
    request: &ApplyPatchRequest,
) -> Result<Vec<ApplyPatchRequest>, MutationError> {
    parse_apply_patch(&request.patch)?;
    let normalized = normalize_newlines(&request.patch);
    let lines = normalized.trim().lines().collect::<Vec<_>>();
    let starts = lines
        .iter()
        .enumerate()
        .skip(1)
        .take(lines.len().saturating_sub(2))
        .filter_map(|(index, line)| {
            let line = line.trim();
            (line.starts_with("*** Add File: ")
                || line.starts_with("*** Delete File: ")
                || line.starts_with("*** Update File: "))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    Ok(starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(lines.len() - 1);
            ApplyPatchRequest {
                patch: std::iter::once("*** Begin Patch")
                    .chain(lines[*start..end].iter().copied())
                    .chain(std::iter::once("*** End Patch"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            }
        })
        .collect())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffFileKind {
    Modified,
    Added,
    Deleted,
    Renamed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u64>,
    pub new_line: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffFile {
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub kind: DiffFileKind,
    pub language: Option<String>,
    pub added_lines: u64,
    pub removed_lines: u64,
    pub old_no_final_newline: bool,
    pub new_no_final_newline: bool,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticDiff {
    pub files: Vec<DiffFile>,
}

/// Recorded successful patches, grouped by file without cancelling later reversions.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationDiff {
    pub diff: SemanticDiff,
    /// Some historical successful results were truncated or unavailable at migration.
    pub incomplete_history: bool,
}

impl SemanticDiff {
    /// Adds a later mutation to this transcript diff, retaining chronological
    /// hunks while presenting repeated edits to one file as one file entry.
    pub fn merge(&mut self, later: Self) {
        for mut later_file in later.files {
            if let Some(file) = self.files.iter_mut().find(|file| {
                file.old_path == later_file.old_path
                    && file.new_path == later_file.new_path
                    && file.kind == later_file.kind
            }) {
                file.added_lines = file.added_lines.saturating_add(later_file.added_lines);
                file.removed_lines = file.removed_lines.saturating_add(later_file.removed_lines);
                file.old_no_final_newline |= later_file.old_no_final_newline;
                file.new_no_final_newline |= later_file.new_no_final_newline;
                file.hunks.append(&mut later_file.hunks);
            } else {
                self.files.push(later_file);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct MutationPlan {
    files: Vec<PlannedFile>,
    pub diff: SemanticDiff,
}

impl MutationPlan {
    #[must_use]
    pub fn affected_paths(&self) -> Vec<&Path> {
        self.files.iter().map(|file| file.path.as_path()).collect()
    }

    /// Splits a multi-file plan into the individual patch actions represented by
    /// its semantic diff. A rename remains one action and keeps both its write
    /// and delete guards together.
    #[must_use]
    pub fn split_actions(&self) -> Vec<Self> {
        self.diff
            .files
            .iter()
            .map(|diff| {
                let paths = [diff.old_path.as_deref(), diff.new_path.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<std::collections::BTreeSet<_>>();
                let files = self
                    .files
                    .iter()
                    .filter(|file| paths.contains(file.path.as_path()))
                    .cloned()
                    .collect();
                Self {
                    files,
                    diff: SemanticDiff {
                        files: vec![diff.clone()],
                    },
                }
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
struct PlannedFile {
    path: PathBuf,
    after: Option<Vec<u8>>,
    guard: FileGuard,
    permissions: Option<FilePermissions>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FilePermissions {
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileGuard {
    exists: bool,
    sha256: Option<String>,
    length: Option<u64>,
    modified_millis: Option<u128>,
    permissions: Option<FilePermissions>,
    #[cfg(unix)]
    device: Option<u64>,
    #[cfg(unix)]
    inode: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationResult {
    pub changed_paths: Vec<PathBuf>,
    pub diff: SemanticDiff,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MutationError {
    #[error("invalid mutation request: {0}")]
    InvalidRequest(String),
    #[error("path is outside the workspace: {0}")]
    ExternalPath(PathBuf),
    #[error("path is not a regular text file: {0}")]
    NotTextFile(PathBuf),
    #[error("file is not valid UTF-8: {0}")]
    InvalidUtf8(PathBuf),
    #[error("file exceeds the mutation size limit: {0}")]
    TooLarge(PathBuf),
    #[error("file changed after its preview was prepared: {0}")]
    FileChanged(PathBuf),
    #[error("edit did not have the expected unique match in {path}: {message}")]
    Match { path: PathBuf, message: String },
    #[error("edit operations overlap in {0}")]
    Overlap(PathBuf),
    #[error("patch is invalid: {0}")]
    InvalidPatch(String),
    #[error("patch partially applied; changed {changed:?}; failed at {failed}: {message}")]
    PartialPatch {
        changed: Vec<PathBuf>,
        failed: PathBuf,
        message: String,
    },
    #[error("I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug)]
pub struct MutationTools {
    workspace: PathBuf,
    allowed_roots: Vec<PathBuf>,
    diff_context_lines: usize,
}

impl MutationTools {
    /// Creates mutation tools rooted at a canonical workspace.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the workspace cannot be canonicalized.
    pub fn new(workspace: &Path) -> Result<Self, MutationError> {
        let workspace = workspace
            .canonicalize()
            .map_err(|source| MutationError::Io {
                path: workspace.to_path_buf(),
                source,
            })?;
        Ok(Self {
            workspace,
            allowed_roots: Vec::new(),
            diff_context_lines: 3,
        })
    }

    /// Adds a canonical root that shares the workspace's external-path boundary.
    pub(crate) fn with_allowed_root(mut self, root: &Path) -> Result<Self, MutationError> {
        let root = root.canonicalize().map_err(|source| MutationError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        self.allowed_roots.push(root);
        Ok(self)
    }

    pub(crate) fn with_optional_allowed_root(
        self,
        root: Option<&Path>,
    ) -> Result<Self, MutationError> {
        match root {
            Some(root) => self.with_allowed_root(root),
            None => Ok(self),
        }
    }

    #[must_use]
    pub fn with_diff_context_lines(mut self, lines: usize) -> Self {
        self.diff_context_lines = lines;
        self
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Resolves a target through its nearest existing ancestor.
    ///
    /// # Errors
    ///
    /// Returns an I/O or validation error when the path cannot be resolved.
    pub fn classify_target(&self, requested: &Path) -> Result<(PathBuf, bool), MutationError> {
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.workspace.join(requested)
        };
        let resolved = resolve_nearest(&candidate)?;
        let outside = !resolved.starts_with(&self.workspace)
            && !self
                .allowed_roots
                .iter()
                .any(|root| resolved.starts_with(root));
        Ok((resolved, outside))
    }

    /// Validates every file and hunk in a Codex-style patch before mutation.
    ///
    /// # Errors
    ///
    /// Returns a structured error for malformed, stale, mismatched, or oversized patches.
    pub fn plan_apply_patch(
        &self,
        request: &ApplyPatchRequest,
    ) -> Result<MutationPlan, MutationError> {
        let parsed = parse_apply_patch(&request.patch)?;
        if parsed.is_empty() {
            return Err(MutationError::InvalidPatch(
                "patch contains no files".into(),
            ));
        }
        let mut files = Vec::with_capacity(parsed.len());
        let mut diffs = Vec::with_capacity(parsed.len());
        let mut touched = std::collections::BTreeSet::new();
        for patch in parsed {
            let (source_path, _) = self.classify_target(&patch.path)?;
            let (target_path, _) =
                self.classify_target(patch.move_path.as_deref().unwrap_or(&patch.path))?;
            if !touched.insert(source_path.clone())
                || (source_path != target_path && !touched.insert(target_path.clone()))
            {
                return Err(MutationError::Overlap(source_path));
            }
            let (before, source_guard) = match &patch.action {
                PatchAction::Add(_) => snapshot_optional(&source_path, EDIT_INPUT_LIMIT)?,
                PatchAction::Delete | PatchAction::Update(_) => {
                    let (bytes, guard) = snapshot_required(&source_path, EDIT_INPUT_LIMIT)?;
                    (Some(bytes), guard)
                }
            };
            let source = before
                .as_deref()
                .map(|bytes| {
                    reject_binary(&source_path, bytes)?;
                    std::str::from_utf8(bytes)
                        .map(str::to_owned)
                        .map_err(|_| MutationError::InvalidUtf8(source_path.clone()))
                })
                .transpose()?
                .unwrap_or_default();
            let after = match &patch.action {
                PatchAction::Add(content) => Some(content.clone().into_bytes()),
                PatchAction::Delete => None,
                PatchAction::Update(chunks) => {
                    let normalized =
                        apply_hunks(&source_path, &normalize_newlines(&source), chunks)?;
                    let converted = apply_line_ending(&normalized, dominant_line_ending(&source));
                    if converted.len() > EDIT_OUTPUT_LIMIT {
                        return Err(MutationError::TooLarge(target_path.clone()));
                    }
                    Some(converted.into_bytes())
                }
            };
            let renamed = patch.move_path.is_some();
            if !renamed && before.as_deref() == after.as_deref() {
                continue;
            }
            let mut diff = build_diff(
                &target_path,
                before.as_deref(),
                after.as_deref(),
                self.diff_context_lines,
            )?;
            if renamed {
                diff.kind = DiffFileKind::Renamed;
                diff.old_path = Some(source_path.clone());
                diff.new_path = Some(target_path.clone());
                let (_, target_guard) = snapshot_optional(&target_path, EDIT_INPUT_LIMIT)?;
                if target_guard.exists {
                    return Err(MutationError::InvalidPatch(format!(
                        "move destination already exists: {}",
                        target_path.display()
                    )));
                }
                files.push(PlannedFile {
                    path: target_path,
                    after,
                    guard: target_guard,
                    permissions: source_guard.permissions.clone(),
                });
                files.push(PlannedFile {
                    path: source_path,
                    after: None,
                    guard: source_guard,
                    permissions: None,
                });
            } else {
                files.push(PlannedFile {
                    path: target_path,
                    after,
                    permissions: source_guard.permissions.clone(),
                    guard: source_guard,
                });
            }
            diffs.push(diff);
        }
        Ok(MutationPlan {
            files,
            diff: SemanticDiff { files: diffs },
        })
    }

    /// Applies a fully planned mutation after rechecking every input snapshot.
    ///
    /// # Errors
    ///
    /// Returns `FileChanged` before side effects or an exact partial-success error after one.
    pub fn apply(&self, plan: MutationPlan) -> Result<MutationResult, MutationError> {
        verify_guards(&plan)?;
        let mut changed = Vec::new();
        for file in &plan.files {
            let result = match &file.after {
                Some(content) => atomic_write(&file.path, content, file.permissions.as_ref()),
                None => std::fs::remove_file(&file.path).map_err(|source| MutationError::Io {
                    path: file.path.clone(),
                    source,
                }),
            };
            if let Err(error) = result {
                if changed.is_empty() {
                    return Err(error);
                }
                return Err(MutationError::PartialPatch {
                    changed,
                    failed: file.path.clone(),
                    message: error.to_string(),
                });
            }
            changed.push(file.path.clone());
        }
        Ok(MutationResult {
            changed_paths: changed,
            diff: plan.diff,
        })
    }

    /// Applies planned writes through a frontend filesystem while retaining
    /// local handling for operations, such as deletion, that ACP v1 cannot express.
    pub async fn apply_with_frontend(
        &self,
        plan: MutationPlan,
        filesystem: &(dyn crate::frontend::FrontendFileSystem + Send + Sync),
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<MutationResult, MutationError> {
        verify_guards(&plan)?;
        let mut changed = Vec::new();
        for file in &plan.files {
            let result = match &file.after {
                Some(content) => {
                    let content = String::from_utf8(content.clone())
                        .map_err(|_| MutationError::InvalidUtf8(file.path.clone()))?;
                    filesystem
                        .write_text_file(
                            crate::frontend::FrontendWriteTextFileRequest {
                                path: file.path.clone(),
                                content,
                            },
                            cancellation.clone(),
                        )
                        .await
                        .map_err(|message| MutationError::Io {
                            path: file.path.clone(),
                            source: std::io::Error::other(message),
                        })
                }
                None => std::fs::remove_file(&file.path).map_err(|source| MutationError::Io {
                    path: file.path.clone(),
                    source,
                }),
            };
            if let Err(error) = result {
                if changed.is_empty() {
                    return Err(error);
                }
                return Err(MutationError::PartialPatch {
                    changed,
                    failed: file.path.clone(),
                    message: error.to_string(),
                });
            }
            changed.push(file.path.clone());
        }
        Ok(MutationResult {
            changed_paths: changed,
            diff: plan.diff,
        })
    }
}

fn verify_guards(plan: &MutationPlan) -> Result<(), MutationError> {
    for file in &plan.files {
        if current_guard(&file.path)? != file.guard {
            return Err(MutationError::FileChanged(file.path.clone()));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum LineEnding {
    Lf,
    CrLf,
}

fn dominant_line_ending(value: &str) -> LineEnding {
    if value.matches("\r\n").count() > value.matches('\n').count() / 2 {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

fn normalize_newlines(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn apply_line_ending(value: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => value.to_owned(),
        LineEnding::CrLf => value.replace('\n', "\r\n"),
    }
}

fn reject_binary(path: &Path, bytes: &[u8]) -> Result<(), MutationError> {
    if bytes.iter().take(64 * 1024).any(|byte| *byte == 0) {
        Err(MutationError::NotTextFile(path.to_path_buf()))
    } else {
        Ok(())
    }
}

fn snapshot_required(path: &Path, limit: u64) -> Result<(Vec<u8>, FileGuard), MutationError> {
    let (bytes, guard) = snapshot_optional(path, limit)?;
    bytes
        .map(|bytes| (bytes, guard))
        .ok_or_else(|| MutationError::NotTextFile(path.to_path_buf()))
}

fn snapshot_optional(
    path: &Path,
    limit: u64,
) -> Result<(Option<Vec<u8>>, FileGuard), MutationError> {
    if !path.exists() {
        return Ok((None, missing_guard()));
    }
    let metadata = path.metadata().map_err(|source| MutationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(MutationError::NotTextFile(path.to_path_buf()));
    }
    if metadata.len() > limit {
        return Err(MutationError::TooLarge(path.to_path_buf()));
    }
    let bytes = std::fs::read(path).map_err(|source| MutationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    reject_binary(path, &bytes)?;
    std::str::from_utf8(&bytes).map_err(|_| MutationError::InvalidUtf8(path.to_path_buf()))?;
    let guard = guard_from(&metadata, &bytes);
    Ok((Some(bytes), guard))
}

fn current_guard(path: &Path) -> Result<FileGuard, MutationError> {
    if !path.exists() {
        return Ok(missing_guard());
    }
    let metadata = path.metadata().map_err(|source| MutationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let bytes = std::fs::read(path).map_err(|source| MutationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(guard_from(&metadata, &bytes))
}

fn guard_from(metadata: &std::fs::Metadata, bytes: &[u8]) -> FileGuard {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;

    FileGuard {
        exists: true,
        sha256: Some(format!("{:x}", Sha256::digest(bytes))),
        length: Some(metadata.len()),
        modified_millis: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis()),
        permissions: Some(permissions_from(&metadata.permissions())),
        #[cfg(unix)]
        device: Some(metadata.dev()),
        #[cfg(unix)]
        inode: Some(metadata.ino()),
    }
}

fn missing_guard() -> FileGuard {
    FileGuard {
        exists: false,
        sha256: None,
        length: None,
        modified_millis: None,
        permissions: None,
        #[cfg(unix)]
        device: None,
        #[cfg(unix)]
        inode: None,
    }
}

fn permissions_from(permissions: &std::fs::Permissions) -> FilePermissions {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    FilePermissions {
        readonly: permissions.readonly(),
        #[cfg(unix)]
        mode: permissions.mode(),
    }
}

fn apply_permissions(file: &std::fs::File, permissions: &FilePermissions) -> std::io::Result<()> {
    let mut file_permissions = file.metadata()?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file_permissions.set_mode(permissions.mode);
    }
    #[cfg(not(unix))]
    file_permissions.set_readonly(permissions.readonly);
    file.set_permissions(file_permissions)
}

fn atomic_write(
    path: &Path,
    content: &[u8],
    permissions: Option<&FilePermissions>,
) -> Result<(), MutationError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| MutationError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| MutationError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    temporary
        .write_all(content)
        .and_then(|()| {
            permissions.map_or(Ok(()), |permissions| {
                apply_permissions(temporary.as_file(), permissions)
            })
        })
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| MutationError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    temporary.persist(path).map_err(|error| MutationError::Io {
        path: path.to_path_buf(),
        source: error.error,
    })?;
    Ok(())
}

fn resolve_nearest(path: &Path) -> Result<PathBuf, MutationError> {
    let mut ancestor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut resolved) => {
                for component in missing.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| MutationError::Io {
                        path: path.to_path_buf(),
                        source: error,
                    })?
                    .to_os_string();
                missing.push(name);
                if !ancestor.pop() {
                    return Err(MutationError::InvalidRequest(format!(
                        "cannot resolve {}",
                        path.display()
                    )));
                }
            }
            Err(source) => {
                return Err(MutationError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
}

fn build_diff(
    path: &Path,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    context: usize,
) -> Result<DiffFile, MutationError> {
    let old_text = old
        .map(|bytes| {
            std::str::from_utf8(bytes).map_err(|_| MutationError::InvalidUtf8(path.to_path_buf()))
        })
        .transpose()?
        .unwrap_or_default();
    let new_text = new
        .map(|bytes| {
            std::str::from_utf8(bytes).map_err(|_| MutationError::InvalidUtf8(path.to_path_buf()))
        })
        .transpose()?
        .unwrap_or_default();
    let old_normalized = normalize_newlines(old_text);
    let new_normalized = normalize_newlines(new_text);
    let old_lines = old_normalized.lines().collect::<Vec<_>>();
    let new_lines = new_normalized.lines().collect::<Vec<_>>();
    let operations = diff_lines(&old_lines, &new_lines);
    let changed = operations
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (line.kind != DiffLineKind::Context).then_some(index))
        .collect::<Vec<_>>();
    let mut ranges = Vec::<(usize, usize)>::new();
    for index in changed {
        let start = index.saturating_sub(context);
        let end = (index + context + 1).min(operations.len());
        if let Some(last) = ranges.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            ranges.push((start, end));
        }
    }
    let hunks = ranges
        .into_iter()
        .map(|(start, end)| {
            let old_start = 1 + operations[..start]
                .iter()
                .filter(|line| line.kind != DiffLineKind::Addition)
                .count();
            let new_start = 1 + operations[..start]
                .iter()
                .filter(|line| line.kind != DiffLineKind::Deletion)
                .count();
            let old_count = operations[start..end]
                .iter()
                .filter(|line| line.kind != DiffLineKind::Addition)
                .count();
            let new_count = operations[start..end]
                .iter()
                .filter(|line| line.kind != DiffLineKind::Deletion)
                .count();
            DiffHunk {
                header: format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@"),
                lines: operations[start..end].to_vec(),
            }
        })
        .collect();
    let added_lines = operations
        .iter()
        .filter(|line| line.kind == DiffLineKind::Addition)
        .count();
    let removed_lines = operations
        .iter()
        .filter(|line| line.kind == DiffLineKind::Deletion)
        .count();
    Ok(DiffFile {
        old_path: old.map(|_| path.to_path_buf()),
        new_path: new.map(|_| path.to_path_buf()),
        kind: match (old, new) {
            (None, Some(_)) => DiffFileKind::Added,
            (Some(_), None) => DiffFileKind::Deleted,
            _ => DiffFileKind::Modified,
        },
        language: crate::presentation::syntax_language_for_path(
            path,
            if new.is_some() { new_text } else { old_text },
        ),
        added_lines: u64::try_from(added_lines).unwrap_or(u64::MAX),
        removed_lines: u64::try_from(removed_lines).unwrap_or(u64::MAX),
        old_no_final_newline: old
            .is_some_and(|_| !old_text.is_empty() && !old_text.ends_with('\n')),
        new_no_final_newline: new
            .is_some_and(|_| !new_text.is_empty() && !new_text.ends_with('\n')),
        hunks,
    })
}

fn diff_lines(old: &[&str], new: &[&str]) -> Vec<DiffLine> {
    const LOOKAHEAD: usize = 200;
    let mut output = Vec::new();
    let (mut old_index, mut new_index) = (0, 0);
    let (mut old_number, mut new_number) = (1_u64, 1_u64);
    while old_index < old.len() || new_index < new.len() {
        if old.get(old_index) == new.get(new_index) && old_index < old.len() {
            output.push(DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(old_number),
                new_line: Some(new_number),
                text: old[old_index].into(),
            });
            old_index += 1;
            new_index += 1;
            old_number += 1;
            new_number += 1;
            continue;
        }
        let mut anchor = None::<(usize, usize)>;
        let old_limit = (old.len() - old_index).min(LOOKAHEAD);
        let new_limit = (new.len() - new_index).min(LOOKAHEAD);
        for distance in 1..=old_limit + new_limit {
            for old_skip in 0..=distance.min(old_limit) {
                let new_skip = distance - old_skip;
                if new_skip <= new_limit
                    && old_index + old_skip < old.len()
                    && new_index + new_skip < new.len()
                    && old[old_index + old_skip] == new[new_index + new_skip]
                {
                    anchor = Some((old_skip, new_skip));
                    break;
                }
            }
            if anchor.is_some() {
                break;
            }
        }
        let (old_skip, new_skip) = anchor.unwrap_or((old.len() - old_index, new.len() - new_index));
        for text in &old[old_index..old_index + old_skip] {
            output.push(DiffLine {
                kind: DiffLineKind::Deletion,
                old_line: Some(old_number),
                new_line: None,
                text: (*text).into(),
            });
            old_number += 1;
        }
        for text in &new[new_index..new_index + new_skip] {
            output.push(DiffLine {
                kind: DiffLineKind::Addition,
                old_line: None,
                new_line: Some(new_number),
                text: (*text).into(),
            });
            new_number += 1;
        }
        old_index += old_skip;
        new_index += new_skip;
    }
    output
}

#[derive(Debug)]
struct ParsedPatchFile {
    path: PathBuf,
    move_path: Option<PathBuf>,
    action: PatchAction,
}

#[derive(Debug)]
enum PatchAction {
    Add(String),
    Delete,
    Update(Vec<ParsedHunk>),
}

#[derive(Debug)]
struct ParsedHunk {
    context: Option<String>,
    line_hint: Option<usize>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    end_of_file: bool,
}

#[allow(clippy::too_many_lines)]
fn parse_apply_patch(source: &str) -> Result<Vec<ParsedPatchFile>, MutationError> {
    let normalized = normalize_newlines(source);
    let lines = normalized.trim().lines().collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some("*** Begin Patch") {
        return Err(MutationError::InvalidPatch(
            "the first line must be '*** Begin Patch'".into(),
        ));
    }
    if lines.last().map(|line| line.trim()) != Some("*** End Patch") {
        return Err(MutationError::InvalidPatch(
            "the last line must be '*** End Patch'".into(),
        ));
    }

    let mut files = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let header = lines[index].trim();
        if let Some(path) = header.strip_prefix("*** Add File: ") {
            let path = parse_patch_path(path, index + 1)?;
            index += 1;
            let mut content = String::new();
            while index + 1 < lines.len() && !is_file_header(lines[index]) {
                let line = lines[index].strip_prefix('+').ok_or_else(|| {
                    patch_line_error(index + 1, "add-file lines must start with '+'")
                })?;
                content.push_str(line);
                content.push('\n');
                index += 1;
            }
            if content.is_empty() {
                return Err(patch_line_error(index + 1, "add-file hunk is empty"));
            }
            files.push(ParsedPatchFile {
                path,
                move_path: None,
                action: PatchAction::Add(content),
            });
        } else if let Some(path) = header.strip_prefix("*** Delete File: ") {
            files.push(ParsedPatchFile {
                path: parse_patch_path(path, index + 1)?,
                move_path: None,
                action: PatchAction::Delete,
            });
            index += 1;
        } else if let Some(path) = header.strip_prefix("*** Update File: ") {
            let path = parse_patch_path(path, index + 1)?;
            index += 1;
            let move_path = lines
                .get(index)
                .and_then(|line| line.trim_end().strip_prefix("*** Move to: "));
            let move_path = move_path
                .map(|path| parse_patch_path(path, index + 1))
                .transpose()?;
            if move_path.is_some() {
                index += 1;
            }
            let mut chunks = Vec::<ParsedHunk>::new();
            while index + 1 < lines.len() && !is_file_header(lines[index]) {
                let line = lines[index];
                let trimmed = line.trim_end();
                if chunks.last().is_some_and(|chunk| chunk.end_of_file) && trimmed.is_empty() {
                    index += 1;
                    continue;
                }
                if trimmed == "@@" || trimmed.starts_with("@@ ") {
                    reject_empty_chunk(chunks.last(), index + 1)?;
                    chunks.push(ParsedHunk {
                        context: trimmed.strip_prefix("@@ ").map(str::to_owned),
                        line_hint: None,
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                        end_of_file: false,
                    });
                } else if let Some((line_hint, context)) = parse_line_hint(trimmed, index + 1)? {
                    reject_empty_chunk(chunks.last(), index + 1)?;
                    chunks.push(ParsedHunk {
                        context: Some(context),
                        line_hint: Some(line_hint),
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                        end_of_file: false,
                    });
                } else if trimmed == "*** End of File" {
                    reject_empty_chunk(chunks.last(), index + 1)?;
                    chunks
                        .last_mut()
                        .ok_or_else(|| {
                            patch_line_error(index + 1, "end-of-file marker has no change chunk")
                        })?
                        .end_of_file = true;
                } else {
                    if chunks.is_empty() {
                        chunks.push(ParsedHunk {
                            context: None,
                            line_hint: None,
                            old_lines: Vec::new(),
                            new_lines: Vec::new(),
                            end_of_file: false,
                        });
                    }
                    let chunk = chunks.last_mut().expect("a chunk was inserted");
                    match line.chars().next() {
                        Some(' ') => {
                            chunk.old_lines.push(line[1..].to_owned());
                            chunk.new_lines.push(line[1..].to_owned());
                        }
                        Some('-') => chunk.old_lines.push(line[1..].to_owned()),
                        Some('+') => chunk.new_lines.push(line[1..].to_owned()),
                        None => {
                            chunk.old_lines.push(String::new());
                            chunk.new_lines.push(String::new());
                        }
                        _ => {
                            return Err(patch_line_error(
                                index + 1,
                                "change lines must start with ' ', '+', or '-'",
                            ));
                        }
                    }
                }
                index += 1;
            }
            reject_empty_chunk(chunks.last(), index + 1)?;
            if chunks.is_empty() {
                return Err(patch_line_error(index + 1, "update-file hunk is empty"));
            }
            files.push(ParsedPatchFile {
                path,
                move_path,
                action: PatchAction::Update(chunks),
            });
        } else {
            return Err(patch_line_error(index + 1, "expected a file hunk header"));
        }
    }
    Ok(files)
}

fn parse_line_hint(value: &str, line: usize) -> Result<Option<(usize, String)>, MutationError> {
    let Some(value) = value.strip_prefix('@') else {
        return Ok(None);
    };
    let Some((number, context)) = value.split_once('@') else {
        return Err(patch_line_error(
            line,
            "location hint must use '@<positive line number>@ <context>'",
        ));
    };
    let context = context
        .strip_prefix(' ')
        .filter(|context| !context.is_empty())
        .ok_or_else(|| {
            patch_line_error(
                line,
                "location hint must include context after '@<positive line number>@'",
            )
        })?;
    let line_hint = number
        .parse::<usize>()
        .ok()
        .filter(|line_hint| *line_hint > 0)
        .ok_or_else(|| {
            patch_line_error(line, "location hint line number must be a positive integer")
        })?;
    Ok(Some((line_hint, context.to_owned())))
}

fn parse_patch_path(value: &str, line: usize) -> Result<PathBuf, MutationError> {
    let value = value.trim();
    if value.is_empty() {
        Err(patch_line_error(line, "file path cannot be empty"))
    } else {
        Ok(PathBuf::from(value))
    }
}

fn is_file_header(line: &str) -> bool {
    let line = line.trim();
    line == "*** End Patch"
        || line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
}

fn reject_empty_chunk(chunk: Option<&ParsedHunk>, line: usize) -> Result<(), MutationError> {
    if chunk.is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty()) {
        Err(patch_line_error(
            line,
            "update chunk does not contain any lines",
        ))
    } else {
        Ok(())
    }
}

fn patch_line_error(line: usize, message: &str) -> MutationError {
    MutationError::InvalidPatch(format!("line {line}: {message}"))
}

fn apply_hunks(path: &Path, source: &str, hunks: &[ParsedHunk]) -> Result<String, MutationError> {
    let mut lines = source.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut replacements = Vec::<(usize, usize, Vec<String>)>::new();
    let mut line_index = 0;
    for hunk in hunks {
        if let Some(context) = &hunk.context {
            let found = hunk
                .line_hint
                .map_or_else(
                    || seek_sequence(&lines, std::slice::from_ref(context), line_index, false),
                    |line_hint| {
                        seek_sequence_with_line_hint(
                            &lines,
                            std::slice::from_ref(context),
                            line_index,
                            line_hint,
                        )
                    },
                )
                .ok_or_else(|| MutationError::Match {
                    path: path.to_path_buf(),
                    message: format!("failed to find context '{context}'"),
                })?;
            line_index = found + 1;
        }
        if hunk.old_lines.is_empty() {
            replacements.push((lines.len(), 0, hunk.new_lines.clone()));
            continue;
        }
        let found = seek_sequence(&lines, &hunk.old_lines, line_index, hunk.end_of_file)
            .ok_or_else(|| MutationError::Match {
                path: path.to_path_buf(),
                message: format!(
                    "failed to find expected lines:\n{}",
                    hunk.old_lines.join("\n")
                ),
            })?;
        replacements.push((found, hunk.old_lines.len(), hunk.new_lines.clone()));
        line_index = found + hunk.old_lines.len();
    }
    for (start, old_len, new_lines) in replacements.into_iter().rev() {
        lines.splice(start..start + old_len, new_lines);
    }
    if !lines.last().is_some_and(String::is_empty) {
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

fn seek_sequence_with_line_hint(
    lines: &[String],
    pattern: &[String],
    start: usize,
    line_hint: usize,
) -> Option<usize> {
    if pattern.is_empty() || pattern.len() > lines.len() {
        return seek_sequence(lines, pattern, start, false);
    }
    let last_start = lines.len() - pattern.len();
    let hinted_index = line_hint - 1;
    let mut candidates = Vec::with_capacity(22);
    candidates.push(hinted_index);
    candidates.extend(hinted_index.saturating_add(1)..=hinted_index.saturating_add(10));
    candidates.extend((hinted_index.saturating_sub(10)..hinted_index).rev());
    candidates.extend(hinted_index.saturating_add(11)..=last_start);

    for index in candidates {
        if index >= start && index <= last_start && sequence_matches(lines, pattern, index) {
            return Some(index);
        }
    }
    seek_sequence(lines, pattern, start, false)
}

fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let search_start = if eof {
        lines.len() - pattern.len()
    } else {
        start
    };
    let end = lines.len() - pattern.len();
    for normalize in [
        identity as fn(&str) -> String,
        trim_end,
        trim,
        normalize_unicode,
    ] {
        for index in search_start..=end {
            if sequence_matches_with(lines, pattern, index, normalize) {
                return Some(index);
            }
        }
    }
    None
}

fn sequence_matches(lines: &[String], pattern: &[String], index: usize) -> bool {
    [
        identity as fn(&str) -> String,
        trim_end,
        trim,
        normalize_unicode,
    ]
    .into_iter()
    .any(|normalize| sequence_matches_with(lines, pattern, index, normalize))
}

fn sequence_matches_with(
    lines: &[String],
    pattern: &[String],
    index: usize,
    normalize: fn(&str) -> String,
) -> bool {
    lines[index..index + pattern.len()]
        .iter()
        .zip(pattern)
        .all(|(actual, expected)| normalize(actual) == normalize(expected))
}

fn identity(value: &str) -> String {
    value.to_owned()
}

fn trim_end(value: &str) -> String {
    value.trim_end().to_owned()
}

fn trim(value: &str) -> String {
    value.trim().to_owned()
}

fn normalize_unicode(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{00a0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200a}' | '\u{202f}' | '\u{205f}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingFrontendFileSystem {
        writes: std::sync::Mutex<Vec<crate::frontend::FrontendWriteTextFileRequest>>,
    }

    impl crate::frontend::FrontendFileSystem for RecordingFrontendFileSystem {
        fn write_text_file(
            &self,
            request: crate::frontend::FrontendWriteTextFileRequest,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>>
        {
            Box::pin(async move {
                self.writes.lock().unwrap().push(request);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn frontend_filesystem_receives_planned_text_write() {
        let temporary = tempfile::tempdir().unwrap();
        let tools = MutationTools::new(temporary.path()).unwrap();
        let path = temporary.path().join("created.txt");
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Add File: created.txt\n+hello\n*** End Patch".into(),
            })
            .unwrap();
        let filesystem = RecordingFrontendFileSystem::default();

        tools
            .apply_with_frontend(
                plan,
                &filesystem,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(!path.exists());
        assert_eq!(
            filesystem.writes.lock().unwrap().as_slice(),
            &[crate::frontend::FrontendWriteTextFileRequest {
                path,
                content: "hello\n".into(),
            }]
        );
    }

    #[test]
    fn allowed_root_shares_external_boundary_without_trusting_siblings() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let scratchpad = temporary.path().join("scratchpad");
        let sibling = temporary.path().join("sibling");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&scratchpad).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let tools = MutationTools::new(&workspace)
            .unwrap()
            .with_allowed_root(&scratchpad)
            .unwrap();

        assert!(
            !tools
                .classify_target(&scratchpad.join("new.txt"))
                .unwrap()
                .1
        );
        assert!(tools.classify_target(&sibling.join("new.txt")).unwrap().1);

        let path = scratchpad.join("notes.txt");
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: format!(
                    "*** Begin Patch\n*** Add File: {}\n+temporary\n*** End Patch",
                    path.display()
                ),
            })
            .unwrap();
        tools.apply(plan).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "temporary\n");
    }

    #[test]
    fn diffs_use_the_shared_syntax_lookup() {
        for (path, source, expected_language) in [
            ("guide.mdx", "# Guide\n", "markdown"),
            ("shader.wgsl", "fn main() {}\n", "wgsl"),
            ("schema.graphql", "type Query { ok: Boolean! }\n", "graphql"),
        ] {
            let diff = build_diff(Path::new(path), None, Some(source.as_bytes()), 3).unwrap();
            assert_eq!(diff.language.as_deref(), Some(expected_language));
        }
        let diff = build_diff(Path::new("data.unknown"), None, Some(b"value\n"), 3).unwrap();
        assert_eq!(diff.language, None);
    }

    #[test]
    fn apply_patch_uses_codex_matching_and_preserves_crlf() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("main.rs");
        std::fs::write(&path, "fn main() {\r\n    call();\r\n}\r\n").unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: main.rs\n@@ fn main() {\n-\tcall();\n+\tchanged();\n*** End Patch".into(),
            })
            .unwrap();
        tools.apply(plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "fn main() {\r\n\tchanged();\r\n}\r\n"
        );
    }

    #[test]
    fn apply_patch_omits_no_op_updates() {
        let temp = tempfile::TempDir::new().unwrap();
        let unchanged = temp.path().join("unchanged.txt");
        let changed = temp.path().join("changed.txt");
        std::fs::write(&unchanged, "same\n").unwrap();
        std::fs::write(&changed, "before\n").unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: unchanged.txt\n-same\n+same\n*** Update File: changed.txt\n-before\n+after\n*** End Patch".into(),
            })
            .unwrap();

        assert_eq!(plan.affected_paths(), vec![changed.as_path()]);
        assert_eq!(plan.diff.files.len(), 1);
        let result = tools.apply(plan).unwrap();
        assert_eq!(result.changed_paths, vec![changed]);
        assert_eq!(std::fs::read_to_string(unchanged).unwrap(), "same\n");
    }

    #[test]
    fn apply_patch_accepts_a_completely_no_op_update() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("unchanged.txt");
        std::fs::write(&path, "same\n").unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: unchanged.txt\n same\n*** End Patch"
                    .into(),
            })
            .unwrap();

        assert!(plan.affected_paths().is_empty());
        assert!(plan.diff.files.is_empty());
        let result = tools.apply(plan).unwrap();
        assert!(result.changed_paths.is_empty());
        assert!(result.diff.files.is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "same\n");
    }

    #[test]
    fn apply_patch_location_hint_prefers_the_hinted_section() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("target.txt");
        std::fs::write(&path, "section\nold\nsection\nold\n").unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: target.txt\n@3@ section\n-old\n+new\n*** End Patch".into(),
            })
            .unwrap();

        tools.apply(plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "section\nold\nsection\nnew\n"
        );
    }

    #[test]
    fn apply_patch_location_hint_searches_nearby_later_sections() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("target.txt");
        std::fs::write(
            &path,
            "section\nold\nfiller\nfiller\nsection\nold\nfiller\nsection\nold\n",
        )
        .unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: target.txt\n@3@ section\n-old\n+new\n*** End Patch".into(),
            })
            .unwrap();

        tools.apply(plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "section\nold\nfiller\nfiller\nsection\nnew\nfiller\nsection\nold\n"
        );
    }

    #[test]
    fn apply_patch_location_hint_searches_later_sections() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("target.txt");
        std::fs::write(
            &path,
            "filler\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nsection\nold\n",
        )
        .unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: target.txt\n@3@ section\n-old\n+new\n*** End Patch".into(),
            })
            .unwrap();

        tools.apply(plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "filler\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nfiller\nsection\nnew\n"
        );
    }

    #[test]
    fn apply_patch_location_hint_searches_nearby_earlier_sections() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("target.txt");
        std::fs::write(
            &path,
            "section\nold\nfiller\nfiller\nfiller\nfiller\nfiller\nsection\nold\n",
        )
        .unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: target.txt\n@10@ section\n-old\n+new\n*** End Patch".into(),
            })
            .unwrap();

        tools.apply(plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "section\nold\nfiller\nfiller\nfiller\nfiller\nfiller\nsection\nnew\n"
        );
    }

    #[test]
    fn apply_patch_location_hint_uses_normalized_matching() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("target.txt");
        std::fs::write(&path, "  section  \n  old  \n").unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: target.txt\n@1@ section\n-old\n+new\n*** End Patch".into(),
            })
            .unwrap();

        tools.apply(plan).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "  section  \nnew\n");
    }

    #[test]
    fn apply_patch_location_hint_falls_back_to_normal_matching() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("target.txt");
        std::fs::write(&path, "section\nold\nfiller\n").unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: target.txt\n@100@ section\n-old\n+new\n*** End Patch".into(),
            })
            .unwrap();

        tools.apply(plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "section\nnew\nfiller\n"
        );
    }

    #[test]
    fn apply_patch_rejects_invalid_location_hints() {
        for header in ["@0@ section", "@321@", "@line@ section"] {
            let error = parse_apply_patch(&format!(
                "*** Begin Patch\n*** Update File: target.txt\n{header}\n-old\n+new\n*** End Patch"
            ))
            .unwrap_err();
            assert!(matches!(error, MutationError::InvalidPatch(_)));
        }
    }

    #[test]
    fn invalid_patch_is_rejected_before_writes() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("a.txt"), "same\n").unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let error = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: a.txt\n-missing\n+new\n*** End Patch"
                    .into(),
            })
            .unwrap_err();
        assert!(matches!(error, MutationError::Match { .. }));
        assert_eq!(
            std::fs::read_to_string(temp.path().join("a.txt")).unwrap(),
            "same\n"
        );
    }

    #[test]
    fn apply_patch_adds_deletes_updates_and_moves_in_one_plan() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("delete.txt"), "gone\n").unwrap();
        std::fs::write(temp.path().join("update.txt"), "old\n").unwrap();
        std::fs::write(temp.path().join("move.txt"), "before\n").unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools.plan_apply_patch(&ApplyPatchRequest { patch: "*** Begin Patch\n*** Add File: add.txt\n+added\n*** Delete File: delete.txt\n*** Update File: update.txt\n-old\n+new\n*** Update File: move.txt\n*** Move to: moved.txt\n-before\n+after\n*** End Patch".into() }).unwrap();
        let actions = plan.split_actions();
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[3].affected_paths().len(), 2);
        tools.apply(plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(temp.path().join("add.txt")).unwrap(),
            "added\n"
        );
        assert!(!temp.path().join("delete.txt").exists());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("update.txt")).unwrap(),
            "new\n"
        );
        assert!(!temp.path().join("move.txt").exists());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("moved.txt")).unwrap(),
            "after\n"
        );
    }

    #[test]
    fn patch_reports_and_applies_rename_metadata() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("old.rs"), "fn old() {}\n").unwrap();
        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools.plan_apply_patch(&ApplyPatchRequest {
            patch: "*** Begin Patch\n*** Update File: old.rs\n*** Move to: new.rs\n-fn old() {}\n+fn new() {}\n*** End Patch".into(),
        }).unwrap();
        assert_eq!(plan.diff.files[0].kind, DiffFileKind::Renamed);
        assert_eq!(
            plan.diff.files[0].old_path.as_deref(),
            Some(temp.path().join("old.rs").as_path())
        );
        tools.apply(plan).unwrap();
        assert!(!temp.path().join("old.rs").exists());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("new.rs")).unwrap(),
            "fn new() {}\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_patch_preserves_permissions_for_updates_and_moves() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::TempDir::new().unwrap();
        let update_path = temp.path().join("update.sh");
        let move_path = temp.path().join("move.sh");
        std::fs::write(&update_path, "#!/bin/sh\necho old\n").unwrap();
        std::fs::write(&move_path, "#!/bin/sh\necho move\n").unwrap();
        std::fs::set_permissions(&update_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&move_path, std::fs::Permissions::from_mode(0o751)).unwrap();

        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: update.sh\n-echo old\n+echo new\n*** Update File: move.sh\n*** Move to: moved.sh\n-echo move\n+echo moved\n*** End Patch".into(),
            })
            .unwrap();
        tools.apply(plan).unwrap();

        assert_eq!(
            std::fs::metadata(&update_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(temp.path().join("moved.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o751
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_patch_rejects_permission_changes_after_preview() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("script.sh");
        std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let tools = MutationTools::new(temp.path()).unwrap();
        let plan = tools
            .plan_apply_patch(&ApplyPatchRequest {
                patch: "*** Begin Patch\n*** Update File: script.sh\n-echo old\n+echo new\n*** End Patch".into(),
            })
            .unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = tools.apply(plan).unwrap_err();

        assert!(matches!(error, MutationError::FileChanged(changed) if changed == path));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#!/bin/sh\necho old\n"
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o7777,
            0o755
        );
    }
}
