//! Frontend-neutral preparation for built-in file and directory views.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use notify::Watcher as _;

use super::{CodeToken, detect_code_language, highlight_code};

pub const FILE_VIEW_MAX_BYTES: u64 = 4 * 1024 * 1024;
pub const IMAGE_VIEW_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const DIRECTORY_VIEW_MAX_ENTRIES: usize = 20_000;
pub const FILE_VIEW_EAGER_HIGHLIGHT_MAX_BYTES: usize = 256 * 1024;
pub const FILE_VIEW_EAGER_HIGHLIGHT_MAX_LINES: usize = 2_000;
const FILESYSTEM_WATCH_DEBOUNCE: Duration = Duration::from_millis(175);
static NEXT_DIRECTORY_TREE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Eq, PartialEq)]
pub enum PathView {
    File(FileView),
    Directory(DirectoryTree),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileView {
    pub path: PathBuf,
    pub bytes: u64,
    pub content: FileViewContent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileViewContent {
    Text {
        language: String,
        lines: Vec<HighlightedLine>,
        highlighting: FileHighlighting,
    },
    Image {
        format: String,
    },
    Binary,
    TooLarge {
        maximum_bytes: u64,
    },
    Unavailable {
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileHighlighting {
    Eager,
    Viewport,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HighlightedLine {
    pub tokens: Vec<CodeToken>,
}

/// A complete post-edit file with removed rows interleaved at their original
/// positions. Frontends can render this like a file while retaining diff
/// meaning without rereading or interpreting hunks themselves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FullFileDiffView {
    pub path: PathBuf,
    pub language: String,
    pub lines: Vec<FullFileDiffLine>,
    pub highlighting: FileHighlighting,
    pub added_lines: u64,
    pub removed_lines: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FullFileDiffLine {
    pub kind: crate::tools::DiffLineKind,
    pub old_line: Option<u64>,
    pub new_line: Option<u64>,
    pub tokens: Vec<CodeToken>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryEntryKind {
    Directory,
    File,
    Symlink,
    Other,
    Loading,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryTreeRow {
    pub path: PathBuf,
    pub name: String,
    pub kind: DirectoryEntryKind,
    pub depth: usize,
    pub guide: String,
    pub expanded: bool,
}

impl DirectoryTreeRow {
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        matches!(self.kind, DirectoryEntryKind::Directory)
    }

    #[must_use]
    pub const fn is_selectable(&self) -> bool {
        !matches!(
            self.kind,
            DirectoryEntryKind::Loading | DirectoryEntryKind::Unavailable
        )
    }

    /// Returns the Nerd Font glyph that best represents this entry.
    #[must_use]
    pub fn icon(&self) -> &'static str {
        directory_entry_icon(&self.name, self.kind, self.expanded)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectoryChildren {
    entries: Vec<DirectoryEntry>,
    truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectoryEntry {
    path: PathBuf,
    name: String,
    kind: DirectoryEntryKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryLoadRequest {
    tree_id: u64,
    path: PathBuf,
    generation: u64,
}

impl DirectoryLoadRequest {
    /// Reads and sorts one directory away from the async runtime's worker threads.
    #[must_use]
    pub async fn load(self) -> DirectoryLoadResult {
        let fallback = self.clone();
        match tokio::task::spawn_blocking(move || self.load_blocking()).await {
            Ok(result) => result,
            Err(error) => DirectoryLoadResult {
                tree_id: fallback.tree_id,
                path: fallback.path,
                generation: fallback.generation,
                outcome: DirectoryLoadOutcome::Unavailable(error.to_string()),
            },
        }
    }

    /// Performs the directory read synchronously. Frontends should normally call [`Self::load`].
    #[must_use]
    pub fn load_blocking(self) -> DirectoryLoadResult {
        let outcome = match read_directory(&self.path) {
            Ok(children) => DirectoryLoadOutcome::Loaded(children),
            Err(error) => DirectoryLoadOutcome::Unavailable(error.to_string()),
        };
        DirectoryLoadResult {
            tree_id: self.tree_id,
            path: self.path,
            generation: self.generation,
            outcome,
        }
    }
}

#[derive(Debug)]
pub struct DirectoryLoadResult {
    tree_id: u64,
    path: PathBuf,
    generation: u64,
    outcome: DirectoryLoadOutcome,
}

impl DirectoryLoadResult {
    #[must_use]
    pub const fn tree_id(&self) -> u64 {
        self.tree_id
    }
}

#[derive(Debug)]
enum DirectoryLoadOutcome {
    Loaded(DirectoryChildren),
    Unavailable(String),
}

/// Lazily loaded directory tree. Only expanded directories are read.
#[derive(Debug, Eq, PartialEq)]
pub struct DirectoryTree {
    id: u64,
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    children: BTreeMap<PathBuf, DirectoryChildren>,
    pending: BTreeMap<PathBuf, u64>,
    queued: Vec<DirectoryLoadRequest>,
    dirty: BTreeSet<PathBuf>,
    errors: BTreeMap<PathBuf, String>,
    next_generation: u64,
    unavailable: Option<String>,
    revision: u64,
}

impl DirectoryTree {
    /// Opens an empty tree and queues its root for background loading.
    ///
    /// # Errors
    /// Returns an I/O error when the root cannot be inspected as a directory.
    pub fn open(root: PathBuf) -> io::Result<Self> {
        let metadata = fs::metadata(&root)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", root.display()),
            ));
        }
        let mut tree = Self {
            id: NEXT_DIRECTORY_TREE_ID.fetch_add(1, Ordering::Relaxed),
            root,
            expanded: BTreeSet::new(),
            children: BTreeMap::new(),
            pending: BTreeMap::new(),
            queued: Vec::new(),
            dirty: BTreeSet::new(),
            errors: BTreeMap::new(),
            next_generation: 0,
            unavailable: None,
            revision: 0,
        };
        let root = tree.root.clone();
        tree.queue_load(root);
        Ok(tree)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Expands or collapses a directory and queues visible branches for background refresh.
    pub fn toggle(&mut self, path: &Path) -> bool {
        if self.expanded.remove(path) {
            self.cancel_hidden_loads();
            self.revision = self.revision.wrapping_add(1);
            return false;
        }
        self.expanded.insert(path.to_path_buf());
        self.queue_visible_refreshes_from(path);
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Queues every currently visible directory for background refresh.
    pub fn refresh(&mut self) {
        let visible = self.visible_directories();
        for path in visible {
            self.queue_load(path);
        }
    }

    /// Queues only visible directories affected by a filesystem event batch.
    pub fn refresh_changed(&mut self, changed_paths: &[PathBuf]) {
        let visible = self.visible_directories();
        for directory in visible {
            if changed_paths.iter().any(|changed| {
                changed == &directory || changed.parent().is_some_and(|parent| parent == directory)
            }) {
                self.queue_load(directory);
            }
        }
    }

    /// Cancels queued and in-flight work logically while retaining cached rows.
    pub fn suspend(&mut self) {
        self.pending.clear();
        self.queued.clear();
        self.dirty.clear();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Returns and clears load work queued by tree operations.
    pub fn take_load_requests(&mut self) -> Vec<DirectoryLoadRequest> {
        std::mem::take(&mut self.queued)
    }

    /// Applies a matching background load result, ignoring stale or foreign results.
    pub fn apply_load_result(&mut self, result: DirectoryLoadResult) -> bool {
        if result.tree_id != self.id || self.pending.get(&result.path) != Some(&result.generation) {
            return false;
        }
        self.pending.remove(&result.path);
        let path = result.path.clone();
        match result.outcome {
            DirectoryLoadOutcome::Loaded(children) => {
                self.errors.remove(&result.path);
                if result.path == self.root {
                    self.unavailable = None;
                }
                self.children.insert(result.path, children);
                self.prune_unreachable();
            }
            DirectoryLoadOutcome::Unavailable(message) => {
                self.children.remove(&result.path);
                self.errors.insert(result.path.clone(), message.clone());
                if result.path == self.root {
                    self.unavailable = Some(message);
                }
            }
        }
        self.revision = self.revision.wrapping_add(1);
        if self.dirty.remove(&path) && self.visible_directories().contains(&path) {
            self.queue_load(path);
        }
        true
    }

    /// Directories whose immediate children are currently visible.
    #[must_use]
    pub fn visible_directories(&self) -> Vec<PathBuf> {
        let mut visible = vec![self.root.clone()];
        self.append_visible_directories(&self.root, &mut visible);
        visible
    }

    #[must_use]
    pub fn rows(&self) -> Vec<DirectoryTreeRow> {
        let mut rows = Vec::new();
        self.append_rows(&self.root, 0, &mut Vec::new(), &mut rows);
        rows
    }

    #[must_use]
    pub fn directory_was_truncated(&self, path: &Path) -> bool {
        self.children
            .get(path)
            .is_some_and(|children| children.truncated)
    }

    #[must_use]
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    fn append_rows(
        &self,
        parent: &Path,
        depth: usize,
        ancestor_has_next: &mut Vec<bool>,
        rows: &mut Vec<DirectoryTreeRow>,
    ) {
        let Some(children) = self.children.get(parent) else {
            self.append_status_row(parent, depth, ancestor_has_next, rows);
            return;
        };
        for (index, entry) in children.entries.iter().enumerate() {
            let has_next = index + 1 < children.entries.len();
            let expanded =
                entry.kind == DirectoryEntryKind::Directory && self.expanded.contains(&entry.path);
            let mut guide = ancestor_has_next
                .iter()
                .map(|has_next| if *has_next { "│  " } else { "   " })
                .collect::<String>();
            guide.push_str(if has_next { "├─ " } else { "└─ " });
            rows.push(DirectoryTreeRow {
                path: entry.path.clone(),
                name: entry.name.clone(),
                kind: entry.kind,
                depth,
                guide,
                expanded,
            });
            if expanded {
                ancestor_has_next.push(has_next);
                self.append_rows(
                    &entry.path,
                    depth.saturating_add(1),
                    ancestor_has_next,
                    rows,
                );
                ancestor_has_next.pop();
            }
        }
    }

    fn append_status_row(
        &self,
        parent: &Path,
        depth: usize,
        ancestor_has_next: &[bool],
        rows: &mut Vec<DirectoryTreeRow>,
    ) {
        let (kind, name) = if self.pending.contains_key(parent) {
            (DirectoryEntryKind::Loading, "Loading…".to_owned())
        } else if let Some(error) = self.errors.get(parent) {
            (
                DirectoryEntryKind::Unavailable,
                format!("Unavailable · {error}"),
            )
        } else {
            return;
        };
        let mut guide = ancestor_has_next
            .iter()
            .map(|has_next| if *has_next { "│  " } else { "   " })
            .collect::<String>();
        guide.push_str("└─ ");
        rows.push(DirectoryTreeRow {
            path: parent.to_path_buf(),
            name,
            kind,
            depth,
            guide,
            expanded: false,
        });
    }

    fn append_visible_directories(&self, parent: &Path, output: &mut Vec<PathBuf>) {
        let Some(children) = self.children.get(parent) else {
            return;
        };
        for entry in &children.entries {
            if entry.kind == DirectoryEntryKind::Directory && self.expanded.contains(&entry.path) {
                output.push(entry.path.clone());
                self.append_visible_directories(&entry.path, output);
            }
        }
    }

    fn queue_visible_refreshes_from(&mut self, root: &Path) {
        let visible = self.visible_directories();
        for path in visible.into_iter().filter(|path| path.starts_with(root)) {
            self.queue_load(path);
        }
    }

    fn queue_load(&mut self, path: PathBuf) {
        if self.pending.contains_key(&path) {
            self.dirty.insert(path);
            return;
        }
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.pending.insert(path.clone(), generation);
        self.queued.retain(|request| request.path != path);
        self.queued.push(DirectoryLoadRequest {
            tree_id: self.id,
            path,
            generation,
        });
    }

    fn cancel_hidden_loads(&mut self) {
        let visible = self
            .visible_directories()
            .into_iter()
            .collect::<BTreeSet<_>>();
        self.pending.retain(|path, _| visible.contains(path));
        self.queued
            .retain(|request| visible.contains(&request.path));
        self.dirty.retain(|path| visible.contains(path));
    }

    fn prune_unreachable(&mut self) {
        let mut reachable = BTreeSet::from([self.root.clone()]);
        let mut pending = vec![self.root.clone()];
        while let Some(parent) = pending.pop() {
            let Some(children) = self.children.get(&parent) else {
                continue;
            };
            for entry in &children.entries {
                if entry.kind == DirectoryEntryKind::Directory
                    && (self.children.contains_key(&entry.path)
                        || self.pending.contains_key(&entry.path)
                        || self.errors.contains_key(&entry.path)
                        || self.expanded.contains(&entry.path))
                    && reachable.insert(entry.path.clone())
                {
                    pending.push(entry.path.clone());
                }
            }
        }
        self.children.retain(|path, _| reachable.contains(path));
        self.expanded.retain(|path| reachable.contains(path));
        self.pending.retain(|path, _| reachable.contains(path));
        self.queued
            .retain(|request| reachable.contains(&request.path));
        self.dirty.retain(|path| reachable.contains(path));
        self.errors.retain(|path, _| reachable.contains(path));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemWatchKind {
    File,
    DirectoryChildren,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemWatchTarget {
    pub path: PathBuf,
    pub kind: FilesystemWatchKind,
}

impl FilesystemWatchTarget {
    #[must_use]
    pub fn file(path: PathBuf) -> Self {
        Self {
            path,
            kind: FilesystemWatchKind::File,
        }
    }

    #[must_use]
    pub fn directory_children(path: PathBuf) -> Self {
        Self {
            path,
            kind: FilesystemWatchKind::DirectoryChildren,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilesystemChange {
    pub paths: Vec<PathBuf>,
    pub errors: Vec<String>,
}

/// A dynamically retargetable, debounced filesystem watcher for frontend views.
pub struct FilesystemWatcher {
    watcher: notify::RecommendedWatcher,
    receiver: tokio::sync::mpsc::Receiver<FilesystemChange>,
    watched: BTreeSet<PathBuf>,
}

impl Drop for FilesystemWatcher {
    fn drop(&mut self) {
        // Wake a debounce worker blocked on the single pending frontend batch
        // before the platform watcher begins shutting down its callbacks.
        self.receiver.close();
    }
}

impl FilesystemWatcher {
    /// Creates an idle watcher. Call [`Self::reconcile`] whenever visible views change.
    ///
    /// # Errors
    /// Returns a notify error if the platform watcher cannot be initialized.
    pub fn new() -> notify::Result<Self> {
        let (raw_sender, raw_receiver) = std::sync::mpsc::sync_channel(256);
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let watcher = notify::recommended_watcher(move |event| {
            let _ = raw_sender.send(event);
        })?;
        std::thread::Builder::new()
            .name("cagent-filesystem-view-watch".into())
            .spawn(move || {
                while let Ok(first) = raw_receiver.recv() {
                    let mut events = vec![first];
                    let deadline = std::time::Instant::now() + FILESYSTEM_WATCH_DEBOUNCE;
                    while let Some(remaining) =
                        deadline.checked_duration_since(std::time::Instant::now())
                    {
                        match raw_receiver.recv_timeout(remaining) {
                            Ok(event) => events.push(event),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let mut paths = BTreeSet::new();
                    let mut errors = Vec::new();
                    for event in events {
                        match event {
                            Ok(event) if !matches!(event.kind, notify::EventKind::Access(_)) => {
                                paths.extend(event.paths);
                            }
                            Ok(_) => {}
                            Err(error) => errors.push(error.to_string()),
                        }
                    }
                    if sender
                        .blocking_send(FilesystemChange {
                            paths: paths.into_iter().collect(),
                            errors,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .map_err(|error| notify::Error::generic(&error.to_string()))?;
        Ok(Self {
            watcher,
            receiver,
            watched: BTreeSet::new(),
        })
    }

    /// Replaces the watched paths with the smallest useful set for the visible views.
    ///
    /// Directory targets watch only their immediate children. Their parent is also watched
    /// so deletion and recreation of the root remain observable. Files watch their containing
    /// directory so atomic-save renames are observed.
    ///
    /// # Errors
    /// Returns a notify error when a requested path cannot be watched.
    pub fn reconcile(&mut self, targets: &[FilesystemWatchTarget]) -> notify::Result<()> {
        let mut requested = BTreeSet::<PathBuf>::new();
        for target in targets {
            match target.kind {
                FilesystemWatchKind::DirectoryChildren if target.path.is_dir() => {
                    requested.insert(target.path.clone());
                    if let Some(parent) = target.path.parent() {
                        requested.insert(parent.to_path_buf());
                    }
                }
                FilesystemWatchKind::File => {
                    if let Some(parent) = target.path.parent() {
                        requested.insert(parent.to_path_buf());
                    }
                }
                FilesystemWatchKind::DirectoryChildren => {
                    if let Some(existing) = closest_existing_ancestor(&target.path) {
                        requested.insert(existing);
                    }
                }
            }
        }

        for path in self
            .watched
            .iter()
            .filter(|path| !requested.contains(*path))
            .cloned()
            .collect::<Vec<_>>()
        {
            let _ = self.watcher.unwatch(&path);
            self.watched.remove(&path);
        }
        for path in &requested {
            if self.watched.contains(path) {
                continue;
            }
            self.watcher
                .watch(path, notify::RecursiveMode::NonRecursive)?;
        }
        self.watched = requested;
        Ok(())
    }

    /// Waits for one coalesced filesystem change batch.
    pub async fn changed(&mut self) -> Option<FilesystemChange> {
        self.receiver.recv().await
    }
}

fn closest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| candidate.exists())
        .map(Path::to_path_buf)
}

/// Returns a Nerd Font icon for a filesystem entry.
#[must_use]
pub fn directory_entry_icon(name: &str, kind: DirectoryEntryKind, expanded: bool) -> &'static str {
    match kind {
        DirectoryEntryKind::Directory if expanded => "",
        DirectoryEntryKind::Directory => "",
        DirectoryEntryKind::Symlink => "",
        DirectoryEntryKind::Other => "󰋔",
        DirectoryEntryKind::Loading => "",
        DirectoryEntryKind::Unavailable => "",
        DirectoryEntryKind::File => file_icon(name),
    }
}

fn file_icon(name: &str) -> &'static str {
    let lowercase = name.to_ascii_lowercase();
    match lowercase.as_str() {
        "cargo.toml" => return "",
        "cargo.lock" => return "",
        "dockerfile"
        | "containerfile"
        | "compose.yml"
        | "compose.yaml"
        | "docker-compose.yml"
        | "docker-compose.yaml"
        | ".dockerignore" => {
            return "󰡨";
        }
        "makefile" | "gnumakefile" => return "",
        "justfile" => return "",
        "cmakelists.txt" => return "",
        ".gitignore" | ".gitattributes" | ".gitmodules" | "commit_editmsg" => return "",
        ".env" | ".env.local" | ".env.development" | ".env.production" => return "",
        "license" | "license.md" | "license.txt" | "copying" | "copying.lesser" => {
            return "";
        }
        "readme" | "readme.md" | "readme.mdx" | "readme.txt" => return "󰂺",
        "package.json" | "package-lock.json" => return "",
        "pnpm-lock.yaml" | "pnpm-workspace.yaml" => return "",
        "yarn.lock" | ".yarnrc" | ".yarnrc.yml" | ".yarnrc.yaml" => return "",
        "bun.lock" | "bun.lockb" => return "",
        "go.mod" | "go.sum" | "go.work" => return "",
        "tsconfig.json" => return "",
        "pom.xml" => return "",
        "gradlew" | "gradle.properties" | "settings.gradle" => return "",
        "wrangler.toml" | "wrangler.json" | "wrangler.jsonc" => return "",
        "vite.config.js" | "vite.config.mjs" | "vite.config.ts" | "vite.config.mts" => {
            return "";
        }
        "next.config.js" | "next.config.mjs" | "next.config.ts" => return "",
        "svelte.config.js" | "svelte.config.ts" => return "",
        "tailwind.config.js" | "tailwind.config.mjs" | "tailwind.config.ts" => return "󱏿",
        _ => {}
    }
    match Path::new(&lowercase)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "rs" | "rlib" => "",
        "js" | "mjs" | "cjs" => "",
        "ts" | "mts" | "cts" => "",
        "jsx" => "",
        "tsx" => "",
        "py" | "pyi" | "pyw" => "",
        "go" => "",
        "c" => "",
        "cc" | "cpp" | "cxx" => "",
        "h" | "hh" | "hpp" | "hxx" => "",
        "java" | "class" | "jar" => "",
        "kt" | "kts" => "",
        "swift" => "",
        "rb" => "",
        "php" => "",
        "lua" => "",
        "sh" | "zsh" | "fish" | "ksh" | "csh" => "",
        "bash" => "",
        "cs" | "csx" => "󰌛",
        "fs" | "fsx" | "fsi" | "fsproj" => "",
        "ex" | "exs" | "eex" | "heex" => "",
        "erl" | "hrl" => "",
        "clj" | "cljc" => "",
        "cljs" => "",
        "dart" => "",
        "hs" | "lhs" => "",
        "scala" | "sbt" => "",
        "jl" => "",
        "r" | "rmd" => "󰟔",
        "pl" | "pm" => "",
        "ps1" | "psm1" | "psd1" => "󰨊",
        "nix" => "",
        "zig" | "zon" => "",
        "nim" | "nimble" => "",
        "gleam" => "",
        "odin" => "󰟢",
        "mojo" => "",
        "astro" => "",
        "sol" => "",
        "tf" => "",
        "tfvars" | "tfstate" => "",
        "wasm" | "wat" => "",
        "vim" | "vimrc" => "",
        "templ" => "",
        "asm" | "s" => "",
        "f" | "f77" | "f90" | "f95" | "f03" | "f08" => "󱈚",
        "coffee" => "",
        "cr" => "",
        "graphql" | "gql" => "",
        "prisma" => "",
        "html" | "htm" => "",
        "css" => "",
        "scss" | "sass" => "",
        "less" => "",
        "vue" => "",
        "svelte" => "",
        "json" | "jsonc" | "json5" | "jsonl" | "cson" => "",
        "toml" => "",
        "yaml" | "yml" => "",
        "ini" | "cfg" | "conf" | "config" | "properties" => "",
        "xml" | "xsl" | "xslt" | "xsd" | "plist" => "󰗀",
        "md" | "mdx" | "markdown" => "",
        "txt" => "󰈙",
        "log" => "󰌱",
        "diff" | "patch" => "",
        "sql" | "db" | "sqlite" | "sqlite3" => "",
        "csv" | "tsv" => "",
        "doc" | "docx" | "odt" | "rtf" => "󰈬",
        "xls" | "xlsx" | "xlsm" | "ods" => "󰈛",
        "ppt" | "pptx" | "pptm" | "odp" => "󰈧",
        "tex" | "sty" | "ltx" => "",
        "typ" => "",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" | "tif" | "tiff"
        | "psd" | "heic" | "heif" | "raw" => "",
        "svg" => "󰜡",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" => "",
        "mp4" | "mkv" | "mov" | "webm" | "avi" | "flv" | "wmv" | "mpeg" | "mpg" => "",
        "zip" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "tar" | "zst" => "",
        "pdf" => "",
        "lock" => "",
        "woff" | "woff2" | "ttf" | "otf" | "eot" => "",
        "apk" => "",
        "exe" | "dll" | "so" | "bin" | "elf" => "",
        "obj" | "stl" | "fbx" | "blend" | "3mf" => "󰆧",
        _ => "󰈔",
    }
}

/// Reads and prepares a path for a built-in viewer.
///
/// # Errors
/// Returns an I/O error when metadata or permitted content cannot be read.
pub fn inspect_path(path: &Path) -> io::Result<PathView> {
    let metadata = fs::metadata(path)?;
    if metadata.is_dir() {
        return DirectoryTree::open(path.to_path_buf()).map(PathView::Directory);
    }
    let bytes = metadata.len();
    let format = image_format(path);
    if let Some(format) = format {
        return Ok(PathView::File(FileView {
            path: path.to_path_buf(),
            bytes,
            content: if bytes > IMAGE_VIEW_MAX_BYTES {
                FileViewContent::TooLarge {
                    maximum_bytes: IMAGE_VIEW_MAX_BYTES,
                }
            } else {
                FileViewContent::Image { format }
            },
        }));
    }
    if bytes > FILE_VIEW_MAX_BYTES {
        return Ok(PathView::File(FileView {
            path: path.to_path_buf(),
            bytes,
            content: FileViewContent::TooLarge {
                maximum_bytes: FILE_VIEW_MAX_BYTES,
            },
        }));
    }
    let source = fs::read(path)?;
    let content = match String::from_utf8(source) {
        Ok(source) if !source.contains('\0') => {
            let language = detect_code_language(path, &source);
            let line_count = source.bytes().filter(|byte| *byte == b'\n').count()
                + usize::from(!source.ends_with('\n') || source.is_empty());
            let highlighting = if source.len() <= FILE_VIEW_EAGER_HIGHLIGHT_MAX_BYTES
                && line_count <= FILE_VIEW_EAGER_HIGHLIGHT_MAX_LINES
            {
                FileHighlighting::Eager
            } else {
                FileHighlighting::Viewport
            };
            let lines = match highlighting {
                FileHighlighting::Eager => highlighted_lines(&language, &source),
                FileHighlighting::Viewport => plain_lines(&source),
            };
            FileViewContent::Text {
                language,
                lines,
                highlighting,
            }
        }
        Ok(_) | Err(_) => FileViewContent::Binary,
    };
    Ok(PathView::File(FileView {
        path: path.to_path_buf(),
        bytes,
        content,
    }))
}

#[must_use]
pub fn unavailable_file_view(path: PathBuf, message: String) -> FileView {
    FileView {
        path,
        bytes: 0,
        content: FileViewContent::Unavailable { message },
    }
}

/// Prepares a file-sized inline diff from a retained mutation hunk and the
/// current post-edit file. Deleted files are recoverable because their
/// mutation diff necessarily retains every old row.
///
/// # Errors
/// Returns an I/O error when the current file cannot be read as displayable
/// text or the retained diff has no usable path.
#[allow(clippy::too_many_lines)]
pub fn inspect_full_file_diff(
    file: &crate::tools::DiffFile,
    workspace: &Path,
) -> io::Result<FullFileDiffView> {
    use crate::tools::{DiffFileKind, DiffLineKind};

    let recorded_path = file
        .new_path
        .as_ref()
        .or(file.old_path.as_ref())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "diff has no file path"))?;
    let path = resolve_display_path(&recorded_path.to_string_lossy(), workspace);
    if file.kind == DiffFileKind::Deleted {
        let language = file.language.clone().unwrap_or_default();
        let mut seen = BTreeSet::new();
        let lines = file
            .hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .filter(|line| line.kind != DiffLineKind::Addition)
            .filter(|line| line.old_line.is_none_or(|number| seen.insert(number)))
            .map(|line| FullFileDiffLine {
                kind: DiffLineKind::Deletion,
                old_line: line.old_line,
                new_line: None,
                tokens: highlight_code(&language, &line.text),
            })
            .collect();
        return Ok(FullFileDiffView {
            path,
            language,
            lines,
            highlighting: FileHighlighting::Eager,
            added_lines: file.added_lines,
            removed_lines: file.removed_lines,
        });
    }

    let PathView::File(view) = inspect_path(&path)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "diff path is not a file",
        ));
    };
    let FileViewContent::Text {
        language,
        lines: current,
        highlighting,
    } = view.content
    else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diff path is not displayable text",
        ));
    };

    let mut deletions = BTreeMap::<usize, Vec<&crate::tools::DiffLine>>::new();
    let mut additions = BTreeMap::<u64, &crate::tools::DiffLine>::new();
    for hunk in &file.hunks {
        for (index, line) in hunk.lines.iter().enumerate() {
            match line.kind {
                DiffLineKind::Addition => {
                    if let Some(number) = line.new_line {
                        additions.insert(number, line);
                    }
                }
                DiffLineKind::Deletion => {
                    let anchor = hunk.lines[index + 1..]
                        .iter()
                        .find_map(|following| following.new_line)
                        .or_else(|| {
                            hunk.lines[..index]
                                .iter()
                                .rev()
                                .find_map(|previous| previous.new_line)
                                .map(|number| number.saturating_add(1))
                        })
                        .unwrap_or(1);
                    deletions
                        .entry(usize::try_from(anchor).unwrap_or(usize::MAX))
                        .or_default()
                        .push(line);
                }
                DiffLineKind::Context => {}
            }
        }
    }

    let retained_removed = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Deletion)
        .count();
    let mut lines = Vec::with_capacity(current.len().saturating_add(retained_removed));
    for (index, current_line) in current.into_iter().enumerate() {
        let number = index.saturating_add(1);
        append_removed_diff_lines(&mut lines, deletions.remove(&number), &language);
        let number_u64 = u64::try_from(number).unwrap_or(u64::MAX);
        let source = current_line
            .tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect::<String>();
        let added = additions
            .get(&number_u64)
            .is_some_and(|line| line.text == source);
        lines.push(FullFileDiffLine {
            kind: if added {
                DiffLineKind::Addition
            } else {
                DiffLineKind::Context
            },
            old_line: None,
            new_line: Some(number_u64),
            tokens: current_line.tokens,
        });
    }
    for (_, removed) in deletions {
        append_removed_diff_lines(&mut lines, Some(removed), &language);
    }
    Ok(FullFileDiffView {
        path,
        language,
        lines,
        highlighting,
        added_lines: file.added_lines,
        removed_lines: file.removed_lines,
    })
}

fn append_removed_diff_lines(
    output: &mut Vec<FullFileDiffLine>,
    removed: Option<Vec<&crate::tools::DiffLine>>,
    language: &str,
) {
    for line in removed.into_iter().flatten() {
        output.push(FullFileDiffLine {
            kind: crate::tools::DiffLineKind::Deletion,
            old_line: line.old_line,
            new_line: None,
            tokens: highlight_code(language, &line.text),
        });
    }
}

/// Resolves a path label produced by transcript presentation back into a local path.
#[must_use]
pub fn resolve_display_path(label: &str, workspace: &Path) -> PathBuf {
    if label == "~" {
        return directories::BaseDirs::new().map_or_else(
            || PathBuf::from(label),
            |dirs| dirs.home_dir().to_path_buf(),
        );
    }
    if let Some(relative) = label.strip_prefix("~/")
        && let Some(dirs) = directories::BaseDirs::new()
    {
        return dirs.home_dir().join(relative);
    }
    let path = PathBuf::from(label);
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

fn highlighted_lines(language: &str, source: &str) -> Vec<HighlightedLine> {
    let mut lines = vec![HighlightedLine::default()];
    for token in highlight_code(language, source) {
        let mut remaining = token.text.as_str();
        while let Some(index) = remaining.find('\n') {
            let before = &remaining[..index];
            if !before.is_empty() {
                lines
                    .last_mut()
                    .expect("one line exists")
                    .tokens
                    .push(CodeToken {
                        kind: token.kind,
                        text: before.to_owned(),
                    });
            }
            lines.push(HighlightedLine::default());
            remaining = &remaining[index + 1..];
        }
        if !remaining.is_empty() {
            lines
                .last_mut()
                .expect("one line exists")
                .tokens
                .push(CodeToken {
                    kind: token.kind,
                    text: remaining.to_owned(),
                });
        }
    }
    if source.ends_with('\n') && lines.len() > 1 {
        lines.pop();
    }
    lines
}

/// Highlights one logical file line for viewport-driven frontends.
#[must_use]
pub fn highlight_file_line(language: &str, source: &str) -> HighlightedLine {
    HighlightedLine {
        tokens: highlight_code(language, source),
    }
}

fn plain_lines(source: &str) -> Vec<HighlightedLine> {
    let mut lines = source
        .split('\n')
        .map(|line| HighlightedLine {
            tokens: (!line.is_empty())
                .then(|| CodeToken {
                    kind: super::CodeTokenKind::Plain,
                    text: line.to_owned(),
                })
                .into_iter()
                .collect(),
        })
        .collect::<Vec<_>>();
    if source.ends_with('\n') && lines.len() > 1 {
        lines.pop();
    }
    lines
}

fn image_format(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    [
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff", "pnm", "pbm", "pgm",
        "ppm", "ff", "qoi", "tga", "dds", "hdr", "exr",
    ]
    .contains(&extension.as_str())
    .then_some(extension)
}

fn read_directory(path: &Path) -> io::Result<DirectoryChildren> {
    let mut entries = Vec::new();
    let mut truncated = false;
    for result in fs::read_dir(path)? {
        if entries.len() == DIRECTORY_VIEW_MAX_ENTRIES {
            truncated = true;
            break;
        }
        let entry = result?;
        let kind = entry.file_type().map(|kind| {
            if kind.is_dir() {
                DirectoryEntryKind::Directory
            } else if kind.is_file() {
                DirectoryEntryKind::File
            } else if kind.is_symlink() {
                DirectoryEntryKind::Symlink
            } else {
                DirectoryEntryKind::Other
            }
        })?;
        entries.push(DirectoryEntry {
            path: entry.path(),
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
        });
    }
    entries.sort_by(|left, right| {
        let left_group = usize::from(left.kind != DirectoryEntryKind::Directory);
        let right_group = usize::from(right.kind != DirectoryEntryKind::Directory);
        left_group
            .cmp(&right_group)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(DirectoryChildren { entries, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_directory_loads(tree: &mut DirectoryTree) {
        loop {
            let requests = tree.take_load_requests();
            if requests.is_empty() {
                break;
            }
            for request in requests {
                let outcome = match read_directory(&request.path) {
                    Ok(children) => DirectoryLoadOutcome::Loaded(children),
                    Err(error) => DirectoryLoadOutcome::Unavailable(error.to_string()),
                };
                assert!(tree.apply_load_result(DirectoryLoadResult {
                    tree_id: request.tree_id,
                    path: request.path,
                    generation: request.generation,
                    outcome,
                }));
            }
        }
    }

    #[test]
    fn text_binary_and_large_files_are_classified_without_unbounded_reads() {
        let temporary = tempfile::tempdir().unwrap();
        let text = temporary.path().join("main.rs");
        fs::write(&text, "fn main() {}\n").unwrap();
        let PathView::File(view) = inspect_path(&text).unwrap() else {
            panic!("expected file");
        };
        let FileViewContent::Text {
            language,
            lines,
            highlighting,
        } = view.content
        else {
            panic!("expected text");
        };
        assert_eq!(language, "rs");
        assert_eq!(lines.len(), 1);
        assert_eq!(highlighting, FileHighlighting::Eager);

        let binary = temporary.path().join("data.bin");
        fs::write(&binary, [0, 1, 2, 255]).unwrap();
        let PathView::File(view) = inspect_path(&binary).unwrap() else {
            panic!("expected file");
        };
        assert_eq!(view.content, FileViewContent::Binary);

        let large = temporary.path().join("large.txt");
        let file = fs::File::create(&large).unwrap();
        file.set_len(FILE_VIEW_MAX_BYTES + 1).unwrap();
        let PathView::File(view) = inspect_path(&large).unwrap() else {
            panic!("expected file");
        };
        assert!(matches!(view.content, FileViewContent::TooLarge { .. }));
    }

    #[test]
    fn ten_thousand_line_text_file_remains_available_for_virtualized_rendering() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("large.rs");
        let source = (0..10_000)
            .map(|line| format!("pub const VALUE_{line}: usize = {line};\n"))
            .collect::<String>();
        fs::write(&path, source).unwrap();

        let PathView::File(view) = inspect_path(&path).unwrap() else {
            panic!("expected file");
        };
        let FileViewContent::Text {
            lines,
            highlighting,
            ..
        } = view.content
        else {
            panic!("expected text");
        };
        assert_eq!(lines.len(), 10_000);
        assert_eq!(highlighting, FileHighlighting::Viewport);
    }

    #[test]
    fn markdown_mdx_and_typescript_file_views_receive_semantic_highlighting() {
        let temporary = tempfile::tempdir().unwrap();
        for (name, source, expected) in [
            (
                "test.md",
                "# Heading\n",
                crate::presentation::CodeTokenKind::MarkupHeading,
            ),
            (
                "test.mdx",
                "# Heading\n",
                crate::presentation::CodeTokenKind::MarkupHeading,
            ),
            (
                "test.ts",
                "const value: string = \"text\";\n",
                crate::presentation::CodeTokenKind::Keyword,
            ),
        ] {
            let path = temporary.path().join(name);
            fs::write(&path, source).unwrap();
            let PathView::File(view) = inspect_path(&path).unwrap() else {
                panic!("expected file");
            };
            let FileViewContent::Text { lines, .. } = view.content else {
                panic!("expected text");
            };
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.tokens)
                    .any(|token| token.kind == expected),
                "{name} did not include {expected:?}"
            );
        }
    }

    #[test]
    fn conventional_filenames_and_shebangs_select_bundled_syntaxes() {
        let temporary = tempfile::tempdir().unwrap();
        for (name, source, expected_language) in [
            (
                "Dockerfile",
                "FROM alpine:latest\nRUN echo hello\n",
                "Dockerfile",
            ),
            (
                "Containerfile",
                "FROM alpine:latest\nRUN echo hello\n",
                "Dockerfile",
            ),
            ("Makefile", "build:\n\tcargo build\n", "Makefile"),
            (".env", "APP_ENV=production\n", "DotENV"),
            ("requirements.txt", "httpx==0.28.1\n", "Requirements.txt"),
            ("Cargo.lock", "version = 4\n", "toml"),
            (
                "script",
                "#!/usr/bin/env python3\ndef main():\n    return 1\n",
                "Python",
            ),
        ] {
            let path = temporary.path().join(name);
            fs::write(&path, source).unwrap();
            let PathView::File(view) = inspect_path(&path).unwrap() else {
                panic!("expected file");
            };
            let FileViewContent::Text {
                language, lines, ..
            } = view.content
            else {
                panic!("expected text");
            };
            assert_eq!(language, expected_language, "wrong syntax for {name}");
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.tokens)
                    .any(|token| token.kind != crate::presentation::CodeTokenKind::Plain),
                "{name} was not semantically highlighted"
            );
        }
    }

    #[test]
    fn directory_tree_loads_children_only_when_expanded() {
        let temporary = tempfile::tempdir().unwrap();
        let nested = temporary.path().join("src");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("lib.rs"), "pub fn value() {}\n").unwrap();
        fs::write(temporary.path().join("README.md"), "hello\n").unwrap();

        let mut tree = DirectoryTree::open(temporary.path().to_path_buf()).unwrap();
        assert!(matches!(
            tree.rows().as_slice(),
            [DirectoryTreeRow {
                kind: DirectoryEntryKind::Loading,
                ..
            }]
        ));
        complete_directory_loads(&mut tree);
        let rows = tree.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "src");
        assert!(rows[0].is_directory());
        assert_eq!(rows[0].guide, "├─ ");
        assert_eq!(rows[0].icon(), "");

        assert!(tree.toggle(&nested));
        assert!(
            tree.rows()
                .iter()
                .any(|row| row.kind == DirectoryEntryKind::Loading)
        );
        complete_directory_loads(&mut tree);
        let rows = tree.rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].name, "lib.rs");
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[0].icon(), "");
        assert_eq!(rows[1].guide, "│  └─ ");
        assert_eq!(rows[1].icon(), "");
        assert_eq!(rows[2].guide, "└─ ");
        assert_eq!(rows[2].icon(), "󰂺");

        assert!(!tree.toggle(&nested));
        assert_eq!(tree.rows().len(), 2);
    }

    #[test]
    fn directory_tree_refresh_preserves_live_expansion_and_recovers_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        let nested = root.join("src");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("lib.rs"), "old\n").unwrap();

        let mut tree = DirectoryTree::open(root.clone()).unwrap();
        complete_directory_loads(&mut tree);
        tree.toggle(&nested);
        complete_directory_loads(&mut tree);
        fs::write(nested.join("new.rs"), "new\n").unwrap();
        tree.refresh();
        complete_directory_loads(&mut tree);
        assert!(tree.rows().iter().any(|row| row.name == "new.rs"));
        assert!(
            tree.rows()
                .iter()
                .any(|row| row.path == nested && row.expanded)
        );

        fs::remove_dir_all(&root).unwrap();
        tree.refresh();
        complete_directory_loads(&mut tree);
        assert!(tree.unavailable().is_some());
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("restored.txt"), "restored\n").unwrap();
        tree.refresh();
        complete_directory_loads(&mut tree);
        assert!(tree.unavailable().is_none());
        assert!(tree.rows().iter().any(|row| row.name == "restored.txt"));
    }

    #[test]
    fn collapsed_branches_cancel_pending_loads_and_stale_results_are_ignored() {
        let temporary = tempfile::tempdir().unwrap();
        let nested = temporary.path().join("src");
        fs::create_dir(&nested).unwrap();
        let mut tree = DirectoryTree::open(temporary.path().to_path_buf()).unwrap();
        complete_directory_loads(&mut tree);

        tree.toggle(&nested);
        assert_eq!(
            tree.visible_directories(),
            vec![temporary.path().to_path_buf(), nested.clone()]
        );
        let request = tree.take_load_requests().pop().unwrap();
        tree.toggle(&nested);
        let result = DirectoryLoadResult {
            tree_id: request.tree_id,
            path: request.path,
            generation: request.generation,
            outcome: DirectoryLoadOutcome::Loaded(DirectoryChildren {
                entries: Vec::new(),
                truncated: false,
            }),
        };
        assert!(!tree.apply_load_result(result));
        assert_eq!(tree.visible_directories(), vec![temporary.path()]);
    }

    #[test]
    fn repeated_refreshes_deduplicate_in_flight_directory_loads() {
        let temporary = tempfile::tempdir().unwrap();
        let mut tree = DirectoryTree::open(temporary.path().to_path_buf()).unwrap();
        let initial = tree.take_load_requests().pop().unwrap();

        tree.refresh();
        tree.refresh();
        assert!(tree.take_load_requests().is_empty());
        assert!(tree.apply_load_result(initial.load_blocking()));
        assert_eq!(tree.take_load_requests().len(), 1);
    }

    #[test]
    fn suspended_trees_ignore_late_results_and_reload_when_resumed() {
        let temporary = tempfile::tempdir().unwrap();
        let mut tree = DirectoryTree::open(temporary.path().to_path_buf()).unwrap();
        let initial = tree.take_load_requests().pop().unwrap();
        tree.suspend();

        assert!(!tree.apply_load_result(initial.load_blocking()));
        tree.refresh();
        assert_eq!(tree.take_load_requests().len(), 1);
    }

    #[tokio::test]
    async fn filesystem_watcher_reports_debounced_directory_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let mut watcher = FilesystemWatcher::new().unwrap();
        watcher
            .reconcile(&[FilesystemWatchTarget::directory_children(
                temporary.path().to_path_buf(),
            )])
            .unwrap();
        assert!(watcher.watched.contains(temporary.path()));
        let changed = temporary.path().join("changed.txt");
        fs::write(&changed, "changed\n").unwrap();

        let update = tokio::time::timeout(Duration::from_secs(3), watcher.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(update.paths.iter().any(|path| path == &changed));
    }

    #[test]
    fn file_icons_cover_special_names_extensions_and_fallbacks() {
        assert_eq!(
            directory_entry_icon("Dockerfile", DirectoryEntryKind::File, false),
            "󰡨"
        );
        assert_eq!(
            directory_entry_icon("app.ts", DirectoryEntryKind::File, false),
            ""
        );
        assert_eq!(
            directory_entry_icon("photo.png", DirectoryEntryKind::File, false),
            ""
        );
        assert_eq!(
            directory_entry_icon("unknown", DirectoryEntryKind::File, false),
            "󰈔"
        );
    }

    #[test]
    fn full_file_diff_interleaves_removed_rows_with_the_current_file() {
        use crate::tools::{DiffFile, DiffFileKind, DiffHunk, DiffLine, DiffLineKind};

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("sample.rs");
        fs::write(&path, "one\ntwo\nnew three\nfour\nfive\n").unwrap();
        let diff = DiffFile {
            old_path: Some(path.clone()),
            new_path: Some(path.clone()),
            kind: DiffFileKind::Modified,
            language: Some("rust".into()),
            added_lines: 1,
            removed_lines: 1,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: vec![DiffHunk {
                header: "@@ -2,3 +2,3 @@".into(),
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Context,
                        old_line: Some(2),
                        new_line: Some(2),
                        text: "two".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Deletion,
                        old_line: Some(3),
                        new_line: None,
                        text: "old three".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Addition,
                        old_line: None,
                        new_line: Some(3),
                        text: "new three".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Context,
                        old_line: Some(4),
                        new_line: Some(4),
                        text: "four".into(),
                    },
                ],
            }],
        };

        let view = inspect_full_file_diff(&diff, temporary.path()).unwrap();
        assert_eq!(view.lines.len(), 6);
        assert_eq!(view.lines[2].kind, DiffLineKind::Deletion);
        assert_eq!(view.lines[2].old_line, Some(3));
        assert_eq!(view.lines[3].kind, DiffLineKind::Addition);
        assert_eq!(view.lines[3].new_line, Some(3));
        assert_eq!(view.lines[5].new_line, Some(5));
    }
}
