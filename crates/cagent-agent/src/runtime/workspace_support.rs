use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

const READ_LINE_LIMIT: u64 = 20_000;
const DEFAULT_ATTACHMENT_BYTE_LIMIT: u64 = 256 * 1024;
const DEFAULT_ATTACHMENT_HARD_LIMIT: u64 = 1024 * 1024;
const DIRECTORY_ATTACHMENT_ENTRY_LIMIT: usize = 250;
const PATH_CANDIDATE_LIMIT: usize = 200_000;

use crate::runtime::workspace::{WorkspaceEntry, WorkspaceEntryKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AttachmentCapture {
    pub path: PathBuf,
    pub content: String,
    pub start_line: u64,
    pub end_line: u64,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum ToolError {
    #[error("invalid tool request: {0}")]
    InvalidRequest(String),
    #[error("path does not exist: {0}")]
    NotFound(PathBuf),
    #[error("path is outside the workspace: requested {requested}, resolved {resolved}")]
    ExternalPath {
        requested: PathBuf,
        resolved: PathBuf,
    },
    #[error("path is not a regular file: {0}")]
    NotFile(PathBuf),
    #[error("file is binary and cannot be read as text: {0}")]
    Binary(PathBuf),
    #[error("file is not valid UTF-8: {0}; use Bash or a binary-aware tool")]
    InvalidUtf8(PathBuf),
    #[error(
        "attachment exceeds the implicit attachment limit; provide an explicit line range: {0}"
    )]
    AttachmentRangeRequired(PathBuf),
    #[error("attachment selection exceeds the attachment capture limit: {0}")]
    AttachmentTooLarge(PathBuf),
    #[error("file changed while its attachment snapshot was being captured: {0}")]
    FileChanged(PathBuf),
    #[error("workspace attachment capture was cancelled")]
    Cancelled,
    #[error("I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone)]
pub(crate) struct WorkspaceSupport {
    workspace: PathBuf,
    allowed_roots: Vec<PathBuf>,
    attachment_byte_limit: u64,
    attachment_hard_limit: u64,
    file_picker_respect_gitignore: bool,
    file_picker_hide_hidden_files: bool,
}

impl std::fmt::Debug for WorkspaceSupport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceSupport")
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

impl WorkspaceSupport {
    /// Creates read-only tools rooted at the canonical workspace directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace does not exist or cannot be canonicalized.
    pub(crate) fn new(workspace: &Path) -> Result<Self, ToolError> {
        let workspace = canonicalize(workspace)?;
        if !workspace.is_dir() {
            return Err(ToolError::InvalidRequest(format!(
                "workspace is not a directory: {}",
                workspace.display()
            )));
        }
        Ok(Self {
            workspace,
            allowed_roots: Vec::new(),
            attachment_byte_limit: DEFAULT_ATTACHMENT_BYTE_LIMIT,
            attachment_hard_limit: DEFAULT_ATTACHMENT_HARD_LIMIT,
            file_picker_respect_gitignore: true,
            file_picker_hide_hidden_files: true,
        })
    }

    pub(crate) fn with_allowed_roots(mut self, roots: &[PathBuf]) -> Result<Self, ToolError> {
        self.allowed_roots = roots
            .iter()
            .map(|root| {
                let root = canonicalize(root)?;
                if !root.is_dir() {
                    return Err(ToolError::InvalidRequest(format!(
                        "additional workspace root is not a directory: {}",
                        root.display()
                    )));
                }
                Ok(root)
            })
            .collect::<Result<_, _>>()?;
        Ok(self)
    }

    #[must_use]
    pub(crate) fn with_attachment_limits(mut self, implicit: u64, hard: u64) -> Self {
        self.attachment_byte_limit = implicit;
        self.attachment_hard_limit = hard;
        self
    }

    #[must_use]
    pub(crate) fn with_file_picker_options(
        mut self,
        respect_gitignore: bool,
        hide_hidden_files: bool,
    ) -> Self {
        self.file_picker_respect_gitignore = respect_gitignore;
        self.file_picker_hide_hidden_files = hide_hidden_files;
        self
    }

    #[must_use]
    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn allowed_roots(&self) -> &[PathBuf] {
        &self.allowed_roots
    }

    /// Returns the visible direct entries used by workspace path completion.
    ///
    /// Hidden entries are filtered before the page limit is applied so they
    /// cannot consume slots that should be shown in the initial completion
    /// page.
    pub(crate) fn visible_workspace_directory(&self) -> Result<Vec<WorkspaceEntry>, ToolError> {
        let mut entries = picker_walk(self, Some(1), &CancellationToken::new())?;
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        entries.truncate(DIRECTORY_ATTACHMENT_ENTRY_LIMIT);
        Ok(entries)
    }

    /// Returns direct visible entries in an explicitly requested directory.
    ///
    /// This is used for interactive completion of home and absolute paths.
    /// The returned paths are absolute; callers may format them using the
    /// syntax the user typed.
    pub(crate) fn visible_directory(&self, root: &Path) -> Result<Vec<WorkspaceEntry>, ToolError> {
        let root = canonicalize(root)?;
        if !root.is_dir() {
            return Err(ToolError::InvalidRequest(format!(
                "path is not a directory: {}",
                root.display()
            )));
        }
        let mut entries = picker_walk_root(self, &root, Some(1), &CancellationToken::new(), false)?;
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        entries.truncate(DIRECTORY_ATTACHMENT_ENTRY_LIMIT);
        Ok(entries)
    }

    pub(crate) fn capture_attachment(
        &self,
        spec: &crate::AttachmentSpec,
        cancellation: &CancellationToken,
    ) -> Result<AttachmentCapture, ToolError> {
        self.capture_attachment_with(spec, cancellation, false, |_| {})
    }

    pub(crate) fn capture_attachment_after_permission(
        &self,
        spec: &crate::AttachmentSpec,
        cancellation: &CancellationToken,
    ) -> Result<AttachmentCapture, ToolError> {
        self.capture_attachment_with(spec, cancellation, true, |_| {})
    }

    fn capture_attachment_with(
        &self,
        spec: &crate::AttachmentSpec,
        cancellation: &CancellationToken,
        external_allowed: bool,
        after_first_read: impl FnOnce(&Path),
    ) -> Result<AttachmentCapture, ToolError> {
        let path = self.resolve_existing_with(&spec.path, external_allowed)?;
        let metadata = path.metadata().map_err(|source| ToolError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.is_dir() {
            if spec.start_line.is_some() || spec.end_line.is_some() {
                return Err(ToolError::InvalidRequest(
                    "directory attachments do not support line ranges".into(),
                ));
            }
            let before = FileFingerprint::from_metadata(&metadata);
            let first = directory_attachment_snapshot(
                self,
                &path,
                self.attachment_byte_limit.min(self.attachment_hard_limit),
                cancellation,
                external_allowed,
            )?;
            after_first_read(&path);
            let middle =
                file_fingerprint(&path).map_err(|_| ToolError::FileChanged(path.clone()))?;
            let second = directory_attachment_snapshot(
                self,
                &path,
                self.attachment_byte_limit.min(self.attachment_hard_limit),
                cancellation,
                external_allowed,
            )
            .map_err(|error| attachment_reread_error(&error, &path))?;
            let after =
                file_fingerprint(&path).map_err(|_| ToolError::FileChanged(path.clone()))?;
            if before != middle || middle != after || first != second {
                return Err(ToolError::FileChanged(path));
            }
            return Ok(first);
        }
        if !metadata.is_file() {
            return Err(ToolError::NotFile(path));
        }
        if spec.start_line.is_none()
            && spec.end_line.is_none()
            && metadata.len() > self.attachment_byte_limit
        {
            return Err(ToolError::AttachmentRangeRequired(path));
        }

        let before = FileFingerprint::from_metadata(&metadata);
        let start = spec.start_line.unwrap_or(1);
        let end = spec.end_line.unwrap_or(u64::MAX);
        if start == 0 || end < start {
            return Err(ToolError::InvalidRequest(
                "attachment line range must be one-based, ordered, and inclusive".into(),
            ));
        }
        let hard_limit = usize::try_from(self.attachment_hard_limit).map_err(|_| {
            ToolError::InvalidRequest("attachment hard limit exceeds this platform's size".into())
        })?;
        let first = read_lines(&path, start, end, hard_limit, cancellation)?;
        after_first_read(&path);
        let middle = file_fingerprint(&path).map_err(|_| ToolError::FileChanged(path.clone()))?;
        let second = read_lines(&path, start, end, hard_limit, cancellation)
            .map_err(|error| attachment_reread_error(&error, &path))?;
        let after = file_fingerprint(&path).map_err(|_| ToolError::FileChanged(path.clone()))?;
        if before != middle || middle != after || first != second {
            return Err(ToolError::FileChanged(path));
        }
        Ok(first)
    }

    /// Returns the complete bounded corpus used by interactive path completion.
    ///
    ///
    /// Unlike [`Self::list`], this is not paginated: completion ranks the entire
    /// corpus locally and applies its own small result limit.
    pub(crate) async fn workspace_path_candidates(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<WorkspaceEntry>, ToolError> {
        let tools = self.clone();
        let cancellation = cancellation.clone();
        tokio::task::spawn_blocking(move || {
            let mut entries = picker_walk(&tools, None, &cancellation)?;
            entries.truncate(PATH_CANDIDATE_LIMIT);
            Ok(entries)
        })
        .await
        .map_err(|error| {
            ToolError::InvalidRequest(format!("workspace path index task failed: {error}"))
        })?
    }

    pub(crate) fn classify_existing(&self, requested: &Path) -> Result<(PathBuf, bool), ToolError> {
        let requested = crate::runtime::worktrees::expand_home_path(requested)
            .map_err(|error| ToolError::InvalidRequest(error.to_string()))?;
        let candidate = if requested.is_absolute() {
            requested
        } else {
            self.workspace.join(&requested)
        };
        let resolved = resolve_nearest(&candidate)?;
        let outside = !resolved.starts_with(&self.workspace)
            && !self
                .allowed_roots
                .iter()
                .any(|root| resolved.starts_with(root));
        Ok((resolved, outside))
    }

    fn resolve_existing_with(
        &self,
        requested: &Path,
        external_allowed: bool,
    ) -> Result<PathBuf, ToolError> {
        let (resolved, outside) = self.classify_existing(requested)?;
        if outside && !external_allowed {
            return Err(ToolError::ExternalPath {
                requested: requested.to_path_buf(),
                resolved,
            });
        }
        if !resolved.exists() {
            return Err(ToolError::NotFound(resolved));
        }
        Ok(resolved)
    }
}

// Kept crate-private while call sites are migrated to the runtime-owned name.
fn picker_walk(
    tools: &WorkspaceSupport,
    max_depth: Option<usize>,
    cancellation: &CancellationToken,
) -> Result<Vec<WorkspaceEntry>, ToolError> {
    picker_walk_root(tools, &tools.workspace, max_depth, cancellation, true)
}

fn picker_walk_root(
    tools: &WorkspaceSupport,
    root: &Path,
    max_depth: Option<usize>,
    cancellation: &CancellationToken,
    workspace_relative_paths: bool,
) -> Result<Vec<WorkspaceEntry>, ToolError> {
    let root = root.to_path_buf();
    let filter_root = root.clone();
    let filter_cancellation = cancellation.clone();
    let mut builder = ignore::WalkBuilder::new(&root);
    builder
        .hidden(tools.file_picker_hide_hidden_files)
        .git_ignore(tools.file_picker_respect_gitignore)
        .git_exclude(tools.file_picker_respect_gitignore)
        .ignore(tools.file_picker_respect_gitignore)
        .require_git(tools.file_picker_respect_gitignore)
        .follow_links(true)
        .max_depth(max_depth)
        .filter_entry(move |entry| {
            if filter_cancellation.is_cancelled() {
                return false;
            }
            if entry.depth() > 0 && entry.file_name() == ".git" {
                return false;
            }
            entry.depth() == 0
                || !entry.path_is_symlink()
                || entry
                    .path()
                    .canonicalize()
                    .is_ok_and(|path| path.starts_with(&filter_root))
        });
    let mut entries = Vec::new();
    for entry in builder.build() {
        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let Ok(entry) = entry else { continue };
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = path.symlink_metadata() else {
            continue;
        };
        let kind = if metadata.file_type().is_symlink() {
            WorkspaceEntryKind::Symlink
        } else if metadata.is_dir() {
            WorkspaceEntryKind::Directory
        } else if metadata.is_file() {
            WorkspaceEntryKind::File
        } else {
            continue;
        };
        entries.push(WorkspaceEntry {
            path: if workspace_relative_paths {
                workspace_relative(&root, path)
            } else {
                path.to_path_buf()
            },
            kind,
        });
        if entries.len() >= PATH_CANDIDATE_LIMIT {
            break;
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries.dedup_by(|left, right| left.path == right.path);
    Ok(entries)
}

fn list_directory(
    tools: &WorkspaceSupport,
    root: &Path,
    external_allowed: bool,
) -> Result<Vec<WorkspaceEntry>, ToolError> {
    let iterator = std::fs::read_dir(root).map_err(|source| ToolError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    iterator
        .map(|entry| {
            let entry = entry.map_err(|source| ToolError::Io {
                path: root.to_path_buf(),
                source,
            })?;
            let requested = entry.path();
            let metadata = requested
                .symlink_metadata()
                .map_err(|source| ToolError::Io {
                    path: requested.clone(),
                    source,
                })?;
            let kind = if metadata.file_type().is_symlink() {
                WorkspaceEntryKind::Symlink
            } else if metadata.is_dir() {
                WorkspaceEntryKind::Directory
            } else {
                WorkspaceEntryKind::File
            };
            let _resolved = tools.resolve_existing_with(&requested, external_allowed)?;
            Ok(WorkspaceEntry {
                path: workspace_relative(&tools.workspace, &requested),
                kind,
            })
        })
        .collect()
}

fn directory_attachment_snapshot(
    tools: &WorkspaceSupport,
    root: &Path,
    byte_limit: u64,
    cancellation: &CancellationToken,
    external_allowed: bool,
) -> Result<AttachmentCapture, ToolError> {
    use std::fmt::Write as _;

    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let mut entries = list_directory(tools, root, external_allowed)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries.dedup_by(|left, right| left.path == right.path);
    if entries.len() > DIRECTORY_ATTACHMENT_ENTRY_LIMIT {
        return Err(ToolError::AttachmentTooLarge(root.to_path_buf()));
    }
    let relative = workspace_relative(&tools.workspace, root);
    let mut content = format!("Directory {}/:\n", relative.display());
    for entry in entries {
        let suffix = match entry.kind {
            WorkspaceEntryKind::Directory => "/",
            WorkspaceEntryKind::File => "",
            WorkspaceEntryKind::Symlink => " (symlink)",
        };
        let _ = writeln!(content, "- {}{suffix}", entry.path.display());
    }
    let content = escape_terminal_controls(&content);
    if u64::try_from(content.len()).unwrap_or(u64::MAX) > byte_limit {
        return Err(ToolError::AttachmentTooLarge(root.to_path_buf()));
    }
    Ok(AttachmentCapture {
        path: root.to_path_buf(),
        start_line: 1,
        end_line: u64::try_from(content.lines().count()).unwrap_or(u64::MAX),
        content,
    })
}

fn workspace_relative(workspace: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(workspace).unwrap_or(path).to_path_buf()
}

#[derive(Eq, PartialEq)]
struct FileFingerprint {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileFingerprint {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;

        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

fn file_fingerprint(path: &Path) -> Result<FileFingerprint, ToolError> {
    path.metadata()
        .map(|metadata| FileFingerprint::from_metadata(&metadata))
        .map_err(|source| ToolError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn attachment_reread_error(error: &ToolError, path: &Path) -> ToolError {
    if matches!(error, ToolError::Cancelled) {
        ToolError::Cancelled
    } else {
        ToolError::FileChanged(path.to_path_buf())
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf, ToolError> {
    path.canonicalize().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ToolError::NotFound(path.to_path_buf())
        } else {
            ToolError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })
}

enum MissingComponent {
    Normal(OsString),
    Parent,
}

fn resolve_nearest(path: &Path) -> Result<PathBuf, ToolError> {
    let mut ancestor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut resolved) => {
                for component in missing.into_iter().rev() {
                    match component {
                        MissingComponent::Normal(name) => resolved.push(name),
                        MissingComponent::Parent => {
                            resolved.pop();
                        }
                    }
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = ancestor
                    .components()
                    .next_back()
                    .ok_or_else(|| ToolError::NotFound(path.to_path_buf()))?;
                match component {
                    std::path::Component::Normal(name) => {
                        missing.push(MissingComponent::Normal(name.to_os_string()));
                    }
                    std::path::Component::ParentDir => missing.push(MissingComponent::Parent),
                    std::path::Component::CurDir => {}
                    std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                        return Err(ToolError::NotFound(path.to_path_buf()));
                    }
                }
                if !ancestor.pop() {
                    return Err(ToolError::NotFound(path.to_path_buf()));
                }
            }
            Err(source) => {
                return Err(ToolError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
}

fn read_lines(
    path: &Path,
    start: u64,
    requested_end: u64,
    byte_limit: usize,
    cancellation: &CancellationToken,
) -> Result<AttachmentCapture, ToolError> {
    let file = File::open(path).map_err(|source| ToolError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let capped_end = requested_end.min(start.saturating_add(READ_LINE_LIMIT - 1));
    let mut selected = Vec::new();
    let mut line_number = 1_u64;
    let mut last_selected = start.saturating_sub(1);
    let limited_by_lines = requested_end > capped_end;
    let mut truncated = false;

    while line_number <= capped_end {
        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let buffer = reader.fill_buf().map_err(|source| ToolError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if buffer.is_empty() {
            break;
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buffer.len(), |index| index + 1);
        if line_number >= start {
            let remaining = byte_limit.saturating_add(4).saturating_sub(selected.len());
            selected.extend_from_slice(&buffer[..consumed.min(remaining)]);
            if consumed > remaining {
                truncated = true;
            }
            last_selected = line_number;
        }
        reader.consume(consumed);
        if newline.is_some() {
            line_number = line_number.saturating_add(1);
        }
        if selected.len() > byte_limit || truncated && selected.len() >= byte_limit {
            truncated = true;
            break;
        }
    }

    if limited_by_lines && !truncated {
        truncated = !reader
            .fill_buf()
            .map_err(|source| ToolError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .is_empty();
    }

    if is_binary(&selected) {
        return Err(ToolError::Binary(path.to_path_buf()));
    }
    if selected.len() > byte_limit {
        selected.truncate(byte_limit);
        if let Err(error) = std::str::from_utf8(&selected) {
            if error.error_len().is_some() {
                return Err(ToolError::InvalidUtf8(path.to_path_buf()));
            }
            selected.truncate(error.valid_up_to());
        }
    }
    let content =
        std::str::from_utf8(&selected).map_err(|_| ToolError::InvalidUtf8(path.to_path_buf()))?;
    if truncated {
        return Err(ToolError::AttachmentTooLarge(path.to_path_buf()));
    }
    Ok(AttachmentCapture {
        path: path.to_path_buf(),
        content: escape_terminal_controls(content),
        start_line: start,
        end_line: last_selected,
    })
}

fn is_binary(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return true;
    }
    let controls = bytes
        .iter()
        .filter(|byte| **byte < 0x20 && !matches!(**byte, b'\n' | b'\t' | b'\r'))
        .count();
    controls > 8 && controls.saturating_mul(10) > bytes.len()
}

fn escape_terminal_controls(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                format!("\\u{{{:x}}}", u32::from(character))
            } else {
                character.to_string()
            }
        })
        .collect()
}

#[cfg(any())]
mod tests {
    use std::fmt::Write as _;
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::{QuestionAnswer, QuestionOption, QuestionPrompt, QuestionRequest, QuestionResult};

    #[test]
    fn reads_ranges_and_escapes_terminal_controls() {
        let temporary = TempDir::new().unwrap();
        fs::write(
            temporary.path().join("text.txt"),
            "one\ntwo\u{1b}[31m\nthree\n",
        )
        .unwrap();
        let tools = ReadOnlyTools::new(temporary.path()).unwrap();
        let cancellation = CancellationToken::new();
        let result = tools
            .read(
                &ReadRequest {
                    path: "text.txt".into(),
                    start_line: Some(2),
                    end_line: Some(3),
                },
                &cancellation,
            )
            .unwrap();
        assert_eq!(result.content, "two\\u{1b}[31m\nthree\n");
        assert_eq!((result.start_line, result.end_line), (2, 3));
        assert!(!result.truncated);
    }

    #[test]
    fn rejects_binary_invalid_utf8_and_implicit_large_reads() {
        let temporary = TempDir::new().unwrap();
        fs::write(temporary.path().join("binary"), b"hello\0world").unwrap();
        fs::write(temporary.path().join("invalid"), [0xff, 0xfe]).unwrap();
        fs::write(
            temporary.path().join("large"),
            vec![b'x'; READ_BYTE_LIMIT + 1],
        )
        .unwrap();
        let tools = ReadOnlyTools::new(temporary.path()).unwrap();
        let cancellation = CancellationToken::new();
        assert!(matches!(
            tools.read(&ReadRequest::file("binary"), &cancellation),
            Err(ToolError::Binary(_))
        ));
        assert!(matches!(
            tools.read(&ReadRequest::file("invalid"), &cancellation),
            Err(ToolError::InvalidUtf8(_))
        ));
        assert!(matches!(
            tools.read(&ReadRequest::file("large"), &cancellation),
            Err(ToolError::RangeRequired(_))
        ));
    }

    #[tokio::test]
    async fn workspace_path_candidate_walk_honors_cancellation() {
        let temporary = TempDir::new().unwrap();
        fs::write(temporary.path().join("visible.txt"), "").unwrap();
        let tools = ReadOnlyTools::new(temporary.path()).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert!(matches!(
            tools.workspace_path_candidates(&cancellation).await,
            Err(ToolError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn workspace_picker_options_control_hidden_and_ignored_paths() {
        let temporary = TempDir::new().unwrap();
        fs::create_dir(temporary.path().join(".git")).unwrap();
        fs::write(temporary.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(temporary.path().join("visible.txt"), "").unwrap();
        fs::write(temporary.path().join("ignored.txt"), "").unwrap();
        fs::write(temporary.path().join(".hidden.txt"), "").unwrap();

        for (respect, hide, ignored_visible, hidden_visible) in [
            (true, true, false, false),
            (true, false, false, true),
            (false, true, true, false),
            (false, false, true, true),
        ] {
            let tools = ReadOnlyTools::new(temporary.path())
                .unwrap()
                .with_file_picker_options(respect, hide);
            let entries = tools
                .workspace_path_candidates(&CancellationToken::new())
                .await
                .unwrap();
            let contains = |name: &str| entries.iter().any(|entry| entry.path == Path::new(name));
            assert!(contains("visible.txt"));
            assert_eq!(contains("ignored.txt"), ignored_visible);
            assert_eq!(contains(".hidden.txt"), hidden_visible);

            let direct = tools.visible_workspace_directory().unwrap();
            let contains = |name: &str| direct.iter().any(|entry| entry.path == Path::new(name));
            assert!(contains("visible.txt"));
            assert_eq!(contains("ignored.txt"), ignored_visible);
            assert_eq!(contains(".hidden.txt"), hidden_visible);
        }
    }

    #[test]
    fn attachment_capture_is_bounded_and_rejects_changed_files() {
        let temporary = TempDir::new().unwrap();
        let soft_limit = usize::try_from(DEFAULT_ATTACHMENT_BYTE_LIMIT).unwrap();
        let oversized = temporary.path().join("oversized.txt");
        fs::write(&oversized, vec![b'x'; soft_limit + 1]).unwrap();
        let changing = temporary.path().join("changing.txt");
        fs::write(&changing, "before\n").unwrap();
        let hard_limit = temporary.path().join("hard-limit.txt");
        fs::write(&hard_limit, vec![b'x'; READ_BYTE_LIMIT + 1]).unwrap();
        let tools = ReadOnlyTools::new(temporary.path()).unwrap();
        let cancellation = CancellationToken::new();

        assert!(matches!(
            tools.read_attachment(&ReadRequest::file("oversized.txt"), &cancellation),
            Err(ToolError::AttachmentRangeRequired(path)) if path == oversized
        ));
        let ranged = tools
            .read_attachment(
                &ReadRequest {
                    path: "oversized.txt".into(),
                    start_line: Some(1),
                    end_line: Some(1),
                },
                &cancellation,
            )
            .unwrap();
        assert_eq!(ranged.content.len(), soft_limit + 1);
        assert!(matches!(
            tools.read_attachment(
                &ReadRequest {
                    path: "hard-limit.txt".into(),
                    start_line: Some(1),
                    end_line: Some(1),
                },
                &cancellation,
            ),
            Err(ToolError::AttachmentTooLarge(path)) if path == hard_limit
        ));
        assert!(matches!(
            tools.read_attachment_with(
                &ReadRequest::file("changing.txt"),
                &cancellation,
                false,
                |path| fs::write(path, "after!\n").unwrap(),
            ),
            Err(ToolError::FileChanged(path)) if path == changing
        ));
    }

    #[test]
    fn directory_attachment_is_non_recursive_bounded_and_stable() {
        let temporary = TempDir::new().unwrap();
        fs::create_dir_all(temporary.path().join("src/nested")).unwrap();
        fs::write(temporary.path().join("src/lib.rs"), "").unwrap();
        fs::write(temporary.path().join("src/nested/hidden.rs"), "").unwrap();
        let tools = ReadOnlyTools::new(temporary.path()).unwrap();
        let cancellation = CancellationToken::new();

        let result = tools
            .read_attachment(&ReadRequest::file("src"), &cancellation)
            .unwrap();
        assert_eq!(
            result.content,
            "Directory src/:\n- src/lib.rs\n- src/nested/\n"
        );
        assert!(!result.content.contains("hidden.rs"));
        assert!(!result.truncated);

        assert!(matches!(
            tools.read_attachment_with(
                &ReadRequest::file("src"),
                &cancellation,
                false,
                |path| fs::write(path.join("new.rs"), "").unwrap(),
            ),
            Err(ToolError::FileChanged(path)) if path == temporary.path().join("src")
        ));
    }

    #[test]
    fn attachment_read_can_cross_the_boundary_only_after_permission() {
        let temporary = TempDir::new().unwrap();
        let workspace = temporary.path().join("workspace");
        let outside = temporary.path().join("outside");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("notes.md"), "notes\n").unwrap();
        let tools = ReadOnlyTools::new(&workspace).unwrap();
        let request = ReadRequest::file(&outside);
        let cancellation = CancellationToken::new();

        assert!(matches!(
            tools.read_attachment(&request, &cancellation),
            Err(ToolError::ExternalPath { .. })
        ));
        let approved = tools
            .read_attachment_after_permission(&request, &cancellation)
            .unwrap();
        assert!(approved.content.contains("notes.md"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_parent_traversal_and_external_symlinks_with_resolved_path() {
        use std::os::unix::fs::symlink;

        let parent = TempDir::new().unwrap();
        let workspace = parent.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let outside = parent.path().join("outside.txt");
        fs::write(&outside, "secret").unwrap();
        symlink(&outside, workspace.join("link.txt")).unwrap();
        let tools = ReadOnlyTools::new(&workspace).unwrap();
        let cancellation = CancellationToken::new();

        for requested in [PathBuf::from("../outside.txt"), PathBuf::from("link.txt")] {
            assert!(matches!(
                tools.read(&ReadRequest::file(requested), &cancellation),
                Err(ToolError::ExternalPath { resolved, .. }) if resolved == outside
            ));
        }
        assert!(matches!(
            tools.read(
                &ReadRequest::file("../missing/place.txt"),
                &cancellation
            ),
            Err(ToolError::ExternalPath { resolved, .. })
                if resolved == parent.path().join("missing/place.txt")
        ));

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        fs::write(workspace.join("local.txt"), "local\n").unwrap();
        assert!(matches!(
            tools.read(&ReadRequest::file("local.txt"), &cancelled),
            Err(ToolError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn lists_directories_and_recursive_rg_globs_with_pagination() {
        let temporary = TempDir::new().unwrap();
        fs::create_dir(temporary.path().join("nested")).unwrap();
        fs::write(temporary.path().join("root.rs"), "fn root() {}\n").unwrap();
        fs::write(temporary.path().join("nested/child.rs"), "fn child() {}\n").unwrap();
        fs::write(temporary.path().join("nested/skip.txt"), "skip\n").unwrap();
        let tools = ReadOnlyTools::new(temporary.path()).unwrap();
        let cancellation = CancellationToken::new();

        let directory = tools
            .list(&ListRequest::directory("."), &cancellation)
            .await
            .unwrap();
        assert!(directory.entries.iter().any(|entry| {
            entry.path == Path::new("nested") && entry.kind == ListEntryKind::Directory
        }));
        let globbed = tools
            .list(
                &ListRequest {
                    path: ".".into(),
                    glob: Some("*.rs".into()),
                    cursor: None,
                },
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(
            globbed
                .entries
                .iter()
                .map(|entry| entry.path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("nested/child.rs"), Path::new("root.rs")]
        );

        for index in 0..=LIST_ENTRY_LIMIT {
            fs::write(temporary.path().join(format!("page-{index:04}")), "x").unwrap();
        }
        let first = tools
            .list(&ListRequest::directory("."), &cancellation)
            .await
            .unwrap();
        assert_eq!(first.entries.len(), LIST_ENTRY_LIMIT);
        assert!(first.truncated);
        let second = tools
            .list(
                &ListRequest {
                    path: ".".into(),
                    glob: None,
                    cursor: first.next_cursor,
                },
                &cancellation,
            )
            .await
            .unwrap();
        assert!(!second.entries.is_empty());
        assert!(!second.truncated);
    }

    #[tokio::test]
    async fn grep_is_bounded_terminal_safe_and_cursor_resumable() {
        let temporary = TempDir::new().unwrap();
        let mut source = String::new();
        for index in 0..=GREP_MATCH_LIMIT {
            writeln!(source, "needle {index}").unwrap();
        }
        source.push_str("needle \u{1b}[31m unsafe\n");
        fs::write(temporary.path().join("matches.txt"), source).unwrap();
        let tools = ReadOnlyTools::new(temporary.path()).unwrap();
        let cancellation = CancellationToken::new();
        let request = GrepRequest {
            pattern: "needle".into(),
            paths: vec!["matches.txt".into()],
            globs: Vec::new(),
            cursor: None,
        };
        let first = tools.grep(&request, &cancellation).await.unwrap();
        assert_eq!(first.matches.len(), GREP_MATCH_LIMIT);
        assert!(first.truncated);
        assert_eq!(first.next_cursor, Some(GREP_MATCH_LIMIT));
        let second = tools
            .grep(
                &GrepRequest {
                    cursor: first.next_cursor,
                    ..request
                },
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(second.matches.len(), 2);
        assert!(!second.truncated);
        assert!(second.matches.last().unwrap().line.contains("\\u{1b}"));
    }

    #[tokio::test]
    async fn external_read_tools_require_permission_and_then_succeed() {
        let parent = TempDir::new().unwrap();
        let workspace = parent.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let outside = parent.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let secret = outside.join("secret.txt");
        fs::write(&secret, "needle").unwrap();
        fs::write(workspace.join("binary"), b"needle\0hidden").unwrap();
        let tools = ReadOnlyTools::new(&workspace).unwrap();
        let cancellation = CancellationToken::new();

        assert!(matches!(
            tools.read(&ReadRequest::file(&secret), &cancellation),
            Err(ToolError::ExternalPath { .. })
        ));
        assert!(matches!(
            tools
                .list(&ListRequest::directory("../outside"), &cancellation)
                .await,
            Err(ToolError::ExternalPath { .. })
        ));

        let read = tools
            .read_after_permission(&ReadRequest::file(&secret), &cancellation)
            .unwrap();
        assert_eq!(read.content, "needle");
        let listed = tools
            .list_after_permission(&ListRequest::directory(&outside), &cancellation)
            .await
            .unwrap();
        assert_eq!(listed.entries.len(), 1);
        assert_eq!(listed.entries[0].path, secret);
        let recursively_listed = tools
            .list_after_permission(
                &ListRequest {
                    path: outside.clone(),
                    glob: Some("*.txt".into()),
                    cursor: None,
                },
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(recursively_listed.entries.len(), 1);
        assert_eq!(recursively_listed.entries[0].path, secret);
        let searched = tools
            .grep_after_permission(
                &GrepRequest {
                    pattern: "needle".into(),
                    paths: vec![outside],
                    globs: Vec::new(),
                    cursor: None,
                },
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(searched.matches.len(), 1);
        assert_eq!(searched.matches[0].path, secret);

        assert!(matches!(
            tools
                .grep(
                    &GrepRequest {
                        pattern: "needle".into(),
                        paths: vec!["../outside/secret.txt".into()],
                        globs: Vec::new(),
                        cursor: None,
                    },
                    &cancellation,
                )
                .await,
            Err(ToolError::ExternalPath { .. })
        ));
        assert!(matches!(
            tools
                .grep(
                    &GrepRequest {
                        pattern: "needle".into(),
                        paths: vec!["binary".into()],
                        globs: Vec::new(),
                        cursor: None,
                    },
                    &cancellation,
                )
                .await,
            Err(ToolError::Binary(_))
        ));

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            tools
                .list(
                    &ListRequest {
                        path: ".".into(),
                        glob: Some("*".into()),
                        cursor: None,
                    },
                    &cancelled,
                )
                .await,
            Err(ToolError::Cancelled)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_and_grep_resolve_external_symlink_targets_before_execution() {
        use std::os::unix::fs::symlink;

        let parent = TempDir::new().unwrap();
        let workspace = parent.path().join("workspace");
        let outside = parent.path().join("outside");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "needle\n").unwrap();
        symlink(&outside, workspace.join("external-dir")).unwrap();
        symlink(outside.join("secret.txt"), workspace.join("external-file")).unwrap();
        let tools = ReadOnlyTools::new(&workspace).unwrap();
        let cancellation = CancellationToken::new();

        assert!(matches!(
            tools
                .list(&ListRequest::directory("external-dir"), &cancellation)
                .await,
            Err(ToolError::ExternalPath { resolved, .. }) if resolved == outside
        ));
        assert!(matches!(
            tools
                .grep(
                    &GrepRequest {
                        pattern: "needle".into(),
                        paths: vec!["external-file".into()],
                        globs: Vec::new(),
                        cursor: None,
                    },
                    &cancellation,
                )
                .await,
            Err(ToolError::ExternalPath { resolved, .. })
                if resolved == outside.join("secret.txt")
        ));
    }

    #[test]
    fn request_schemas_reject_unknown_fields() {
        let error = serde_json::from_value::<GrepRequest>(serde_json::json!({
            "pattern": "needle",
            "paths": [],
            "globs": [],
            "cursor": null,
            "surprise": true
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rendered_grep_lines_stop_on_utf8_boundaries() {
        let value = format!("{}🦀", "x".repeat(GREP_LINE_LIMIT - 1));
        let (truncated, did_truncate) = truncate_utf8(&value, GREP_LINE_LIMIT);
        assert_eq!(truncated.len(), GREP_LINE_LIMIT - 1);
        assert!(did_truncate);
    }

    #[test]
    fn question_request_requires_small_keyed_nonempty_form() {
        let valid = QuestionRequest {
            questions: vec![QuestionPrompt {
                id: "scope".into(),
                header: "Scope".into(),
                question: "Where should this apply?".into(),
                options: vec![
                    QuestionOption {
                        label: "Root".into(),
                        description: "Only the main agent".into(),
                    },
                    QuestionOption {
                        label: "All agents".into(),
                        description: "Root and delegated agents".into(),
                    },
                ],
            }],
        };
        assert!(valid.validate().is_ok());

        let mut duplicate = valid.clone();
        duplicate.questions.push(duplicate.questions[0].clone());
        assert!(duplicate.validate().unwrap_err().contains("unique"));

        let mut blank = valid;
        blank.questions[0].options[0].description.clear();
        assert!(blank.validate().unwrap_err().contains("non-empty"));
    }

    #[test]
    fn question_result_serializes_structured_optional_answers() {
        let result = QuestionResult {
            answers: [(
                "scope".into(),
                QuestionAnswer {
                    selection: Some("All agents".into()),
                    note: Some("Keep profile policy controls.".into()),
                },
            )]
            .into_iter()
            .collect(),
            cancelled: false,
        };
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({"answers": {"scope": {
                "selection": "All agents", "note": "Keep profile policy controls."
            }}})
        );
    }
}
