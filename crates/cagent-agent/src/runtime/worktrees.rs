use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};

use crate::config::{WorktreeConfig, WorktreeCopy};
use crate::{RuntimeError, WorkspaceVcs, WorktreeMetadata};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeWorkingDirectoryRequest {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnterWorktreeRequest {
    pub name: Option<String>,
    pub path: Option<PathBuf>,
    pub base: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkspaceTransition {
    pub project_dir: PathBuf,
    pub previous_cwd: PathBuf,
    pub cwd: PathBuf,
    pub reason: String,
    pub vcs: Option<WorkspaceVcs>,
    pub name: Option<String>,
    pub branch_or_workspace: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    pub created: bool,
    pub warning: Option<String>,
    pub worktree: Option<WorktreeMetadata>,
}

#[derive(Clone, Debug)]
struct Repository {
    vcs: WorkspaceVcs,
    root: PathBuf,
}

/// One Git worktree or Jujutsu workspace belonging to the current repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeInfo {
    pub name: String,
    pub path: PathBuf,
    pub root: bool,
    pub current: bool,
}

pub(super) fn list(workspace: &Path) -> Result<Vec<WorktreeInfo>, RuntimeError> {
    let current = workspace.canonicalize()?;
    let repository = detect_repository(workspace)?;
    match repository.vcs {
        WorkspaceVcs::Git => {
            let output = command(
                "git",
                &["worktree", "list", "--porcelain"],
                &repository.root,
            )?;
            if !output.status.success() {
                return Err(command_error("git worktree list --porcelain", &output));
            }
            Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.strip_prefix("worktree "))
                .map(PathBuf::from)
                .enumerate()
                .map(|(index, path)| WorktreeInfo {
                    name: if index == 0 {
                        "Root".into()
                    } else {
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    },
                    current: path.canonicalize().is_ok_and(|path| path == current),
                    path,
                    root: index == 0,
                })
                .collect())
        }
        WorkspaceVcs::Jujutsu => {
            let output = command("jj", &["workspace", "list"], &repository.root)?;
            if !output.status.success() {
                return Err(command_error("jj workspace list", &output));
            }
            output
                .stdout
                .split(|byte| *byte == b'\n')
                .filter_map(|line| {
                    String::from_utf8_lossy(line)
                        .split_once(':')
                        .map(|row| row.0.trim().trim_end_matches('@').to_owned())
                })
                .filter(|name| !name.is_empty())
                .map(|name| {
                    let root = command(
                        "jj",
                        &["workspace", "root", "--name", &name],
                        &repository.root,
                    )?;
                    if !root.status.success() {
                        return Err(command_error("jj workspace root", &root));
                    }
                    let path = PathBuf::from(trim_output(&root)?).canonicalize()?;
                    Ok(WorktreeInfo {
                        name: if name == "default" {
                            "Root".into()
                        } else {
                            name.clone()
                        },
                        current: path == current,
                        path,
                        root: name == "default",
                    })
                })
                .collect()
        }
    }
}

pub fn prepare_directory_change(
    project_dir: &Path,
    cwd: &Path,
    requested: &Path,
) -> Result<WorkspaceTransition, RuntimeError> {
    let requested = if requested == Path::new(".") {
        project_dir.to_path_buf()
    } else if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        cwd.join(requested)
    };
    let target = requested.canonicalize().map_err(|error| {
        RuntimeError::InvalidOption(format!(
            "working directory does not exist or cannot be accessed: {}: {error}",
            requested.display()
        ))
    })?;
    if !target.is_dir() {
        return Err(RuntimeError::InvalidOption(format!(
            "working directory is not a directory: {}",
            target.display()
        )));
    }
    let vcs = detect_repository(&target)
        .ok()
        .map(|repository| repository.vcs);
    Ok(WorkspaceTransition {
        project_dir: project_dir.to_path_buf(),
        previous_cwd: cwd.to_path_buf(),
        cwd: target,
        reason: "working_directory_changed".into(),
        vcs,
        name: None,
        branch_or_workspace: None,
        base: None,
        created: false,
        warning: None,
        worktree: None,
    })
}

/// Expands a current-user home prefix for an interactive directory request.
/// Named-user forms are deliberately unsupported so resolution is portable.
pub fn expand_home_path(requested: &Path) -> Result<PathBuf, RuntimeError> {
    let mut components = requested.components();
    let Some(std::path::Component::Normal(first)) = components.next() else {
        return Ok(requested.to_path_buf());
    };
    let first = first.to_string_lossy();
    if first == "~" {
        let home = directories::BaseDirs::new()
            .map(|directories| directories.home_dir().to_path_buf())
            .ok_or_else(|| RuntimeError::InvalidOption("home directory is unavailable".into()))?;
        return Ok(components.fold(home, |path, component| path.join(component.as_os_str())));
    }
    if first.starts_with('~') {
        return Err(RuntimeError::InvalidOption(
            "named-user home paths are unsupported; use ~ or ~/path".into(),
        ));
    }
    Ok(requested.to_path_buf())
}

/// Formats an absolute path, shortening the current user's home prefix.
#[must_use]
pub fn display_absolute_path(path: &Path) -> String {
    if let Some(home) =
        directories::BaseDirs::new().map(|directories| directories.home_dir().to_path_buf())
        && let Ok(relative) = path.strip_prefix(home)
    {
        return if relative.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", relative.to_string_lossy().replace('\\', "/"))
        };
    }
    path.to_string_lossy().replace('\\', "/")
}

/// Resolves the location an enter request may read or create before any VCS
/// mutation occurs. Callers use this to authorize external targets.
pub fn prospective_worktree_path(
    project_dir: &Path,
    cwd: &Path,
    request: &EnterWorktreeRequest,
) -> Result<PathBuf, RuntimeError> {
    match (&request.name, &request.path) {
        (Some(_), Some(_)) => Err(RuntimeError::InvalidOption(
            "enter_worktree accepts name or path, not both".into(),
        )),
        (_, Some(path)) if path == Path::new(".") => Ok(project_dir.to_path_buf()),
        (_, Some(path)) if path.is_absolute() => Ok(path.to_path_buf()),
        (_, Some(path)) => Ok(cwd.join(path)),
        (name, None) => {
            if let Some(name) = name {
                validate_name(name)?;
            }
            let repository = detect_repository(project_dir)?;
            let managed = repository.root.join(".cagent").join("worktrees");
            Ok(name
                .as_ref()
                .map_or(managed.clone(), |name| managed.join(name)))
        }
    }
}

pub fn prepare_worktree_entry(
    project_dir: &Path,
    cwd: &Path,
    request: &EnterWorktreeRequest,
    config: &WorktreeConfig,
    conversation_name: Option<&str>,
) -> Result<WorkspaceTransition, RuntimeError> {
    if request.name.is_some() && request.path.is_some() {
        return Err(RuntimeError::InvalidOption(
            "enter_worktree accepts name or path, not both".into(),
        ));
    }
    if request.path.as_deref() == Some(Path::new(".")) {
        return prepare_directory_change(project_dir, cwd, project_dir);
    }
    let repository = detect_repository(project_dir)?;
    let managed_root = repository.root.join(".cagent").join("worktrees");
    let (name, target) = match (&request.name, &request.path) {
        (Some(name), None) => {
            validate_name(name)?;
            (name.clone(), managed_root.join(name))
        }
        (None, Some(path)) => {
            let target = if path.is_absolute() {
                path.clone()
            } else {
                cwd.join(path)
            };
            let name = target
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    RuntimeError::InvalidOption("worktree path must have a UTF-8 file name".into())
                })?
                .to_owned();
            validate_name(&name)?;
            (name, target)
        }
        (None, None) => generated_name(&managed_root)?,
        (Some(_), Some(_)) => unreachable!(),
    };
    let target = absolute_without_existing_leaf(&target)?;
    let base = request.base.as_deref().unwrap_or(&config.base);
    if target.exists() {
        let target = target.canonicalize()?;
        verify_existing(&repository, &target)?;
        let binding = binding_name(&repository, &target, &name)?;
        let metadata = WorktreeMetadata {
            vcs: repository.vcs,
            name: name.clone(),
            branch_or_workspace: binding.clone(),
            path: target.clone(),
        };
        return Ok(WorkspaceTransition {
            project_dir: project_dir.to_path_buf(),
            previous_cwd: cwd.to_path_buf(),
            cwd: target,
            reason: "worktree_entered".into(),
            vcs: Some(repository.vcs),
            name: Some(name),
            branch_or_workspace: Some(binding),
            base: None,
            created: false,
            warning: None,
            worktree: Some(metadata),
        });
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (binding, warning) = match repository.vcs {
        WorkspaceVcs::Git => create_git(&repository, cwd, &target, &name, base, config)?,
        WorkspaceVcs::Jujutsu => {
            create_jj(&repository, cwd, &target, &name, base, conversation_name)?
        }
    };
    if config.copy != WorktreeCopy::Off
        && let Err(error) = copy_ignored(&repository.root, &target, config.copy)
    {
        return Err(RuntimeError::InvalidOption(format!(
            "created worktree at {} but ignored-file copying failed: {error}",
            target.display()
        )));
    }
    let target = target.canonicalize()?;
    verify_existing(&repository, &target).map_err(|error| {
        RuntimeError::InvalidOption(format!(
            "created worktree at {} but could not enter it: {error}",
            target.display()
        ))
    })?;
    let metadata = WorktreeMetadata {
        vcs: repository.vcs,
        name: name.clone(),
        branch_or_workspace: binding.clone(),
        path: target.clone(),
    };
    Ok(WorkspaceTransition {
        project_dir: project_dir.to_path_buf(),
        previous_cwd: cwd.to_path_buf(),
        cwd: target,
        reason: "worktree_created".into(),
        vcs: Some(repository.vcs),
        name: Some(name),
        branch_or_workspace: Some(binding),
        base: Some(base.to_owned()),
        created: true,
        warning,
        worktree: Some(metadata),
    })
}

fn generated_name(managed_root: &Path) -> Result<(String, PathBuf), RuntimeError> {
    for _ in 0..32 {
        let name = uuid::Uuid::now_v7().simple().to_string()[..8].to_owned();
        let target = managed_root.join(&name);
        if !target.exists() {
            return Ok((name, target));
        }
    }
    Err(RuntimeError::InvalidOption(
        "could not generate an unused worktree name after 32 attempts".into(),
    ))
}

fn validate_name(name: &str) -> Result<(), RuntimeError> {
    let valid = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|character| {
            character.is_control()
                || matches!(character, '/' | '\\')
                || !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        });
    if valid {
        Ok(())
    } else {
        Err(RuntimeError::InvalidOption(format!(
            "invalid worktree name {name:?}; use ASCII letters, digits, '.', '-', or '_'"
        )))
    }
}

fn absolute_without_existing_leaf(path: &Path) -> Result<PathBuf, RuntimeError> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let parent = path.parent().ok_or_else(|| {
        RuntimeError::InvalidOption(format!("worktree path has no parent: {}", path.display()))
    })?;
    let parent = parent.canonicalize().or_else(|_| {
        std::fs::create_dir_all(parent)?;
        parent.canonicalize()
    })?;
    Ok(parent.join(
        path.file_name()
            .ok_or_else(|| RuntimeError::InvalidOption("worktree path has no file name".into()))?,
    ))
}

fn detect_repository(path: &Path) -> Result<Repository, RuntimeError> {
    let path = path.canonicalize()?;
    if let Some(repository) = repository_marker(&path) {
        if repository.vcs == WorkspaceVcs::Jujutsu {
            let root = repository.root;
            ensure_command("jj", &["--version"], &root)?;
            return Ok(Repository {
                vcs: WorkspaceVcs::Jujutsu,
                root,
            });
        }
        let output = command("git", &["rev-parse", "--show-toplevel"], &path)?;
        if output.status.success() {
            return Ok(Repository {
                vcs: WorkspaceVcs::Git,
                root: PathBuf::from(trim_output(&output)?).canonicalize()?,
            });
        }
    }
    Err(RuntimeError::InvalidOption(format!(
        "directory is not inside a Git or Jujutsu repository: {}",
        path.display()
    )))
}

fn repository_marker(path: &Path) -> Option<Repository> {
    let path = path.canonicalize().ok()?;
    path.ancestors().find_map(|root| {
        if root.join(".jj").exists() {
            Some(Repository {
                vcs: WorkspaceVcs::Jujutsu,
                root: root.to_path_buf(),
            })
        } else if root.join(".git").exists() {
            Some(Repository {
                vcs: WorkspaceVcs::Git,
                root: root.to_path_buf(),
            })
        } else {
            None
        }
    })
}

/// Formats a worktree target consistently for approvals and transcript notices.
#[must_use]
pub fn display_worktree_target(project_dir: &Path, current_dir: &Path, target: &Path) -> String {
    let managed_root =
        repository_marker(project_dir).map(|repository| repository.root.join(".cagent/worktrees"));
    if let Some(name) = managed_root
        .as_deref()
        .and_then(|root| target.strip_prefix(root).ok())
        .filter(|relative| relative.components().count() == 1)
    {
        return name.to_string_lossy().replace('\\', "/");
    }
    display_directory_target(project_dir, current_dir, target)
}

/// Formats a cwd transition target relative to the previous cwd when both
/// locations remain inside the immutable launch project.
#[must_use]
pub fn display_directory_target(project_dir: &Path, current_dir: &Path, target: &Path) -> String {
    if current_dir.starts_with(project_dir)
        && target.starts_with(project_dir)
        && let Some(relative) = relative_path(current_dir, target)
    {
        return relative.to_string_lossy().replace('\\', "/");
    }
    display_absolute_path(target)
}

fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    let mut relative = PathBuf::new();
    for _ in common..from.len() {
        relative.push("..");
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Some(relative)
}

fn create_git(
    repository: &Repository,
    cwd: &Path,
    target: &Path,
    name: &str,
    requested_base: &str,
    config: &WorktreeConfig,
) -> Result<(String, Option<String>), RuntimeError> {
    let branch = if config.branch_prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{}-{name}", config.branch_prefix)
    };
    let (base, warning) = match requested_base {
        "head" => ("HEAD".to_owned(), None),
        "fresh" => {
            let fetch = command("git", &["fetch", "origin"], cwd)?;
            let remote = command(
                "git",
                &["rev-parse", "--verify", "refs/remotes/origin/HEAD"],
                cwd,
            )?;
            if fetch.status.success() && remote.status.success() {
                ("refs/remotes/origin/HEAD".to_owned(), None)
            } else {
                (
                    "HEAD".to_owned(),
                    Some(
                        "Could not refresh or resolve origin's default branch; used local HEAD."
                            .into(),
                    ),
                )
            }
        }
        other => {
            ensure_command(
                "git",
                &["rev-parse", "--verify", &format!("{other}^{{commit}}")],
                cwd,
            )?;
            (other.to_owned(), None)
        }
    };
    let target_string = target.to_string_lossy().into_owned();
    ensure_command(
        "git",
        &["worktree", "add", "-b", &branch, &target_string, &base],
        &repository.root,
    )?;
    Ok((branch, warning))
}

fn create_jj(
    repository: &Repository,
    cwd: &Path,
    target: &Path,
    name: &str,
    requested_base: &str,
    conversation_name: Option<&str>,
) -> Result<(String, Option<String>), RuntimeError> {
    let (base, warning) = match requested_base {
        "head" => ("@".to_owned(), None),
        "fresh" => {
            let fetch = command("jj", &["git", "fetch", "--remote", "origin"], cwd)?;
            let trunk = command(
                "jj",
                &["log", "-r", "trunk()", "--no-graph", "-T", "commit_id"],
                cwd,
            )?;
            if fetch.status.success() && trunk.status.success() {
                ("trunk()".to_owned(), None)
            } else {
                (
                    "@".to_owned(),
                    Some(
                        "Could not refresh or resolve trunk(); used the current JJ revision."
                            .into(),
                    ),
                )
            }
        }
        other => (other.to_owned(), None),
    };
    let target_string = target.to_string_lossy().into_owned();
    let arguments = jj_workspace_add_arguments(name, &base, &target_string, conversation_name);
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    ensure_command("jj", &arguments, &repository.root)?;
    Ok((name.to_owned(), warning))
}

fn jj_workspace_add_arguments(
    name: &str,
    base: &str,
    target: &str,
    conversation_name: Option<&str>,
) -> Vec<String> {
    let mut arguments = vec![
        "workspace".into(),
        "add".into(),
        "--name".into(),
        name.into(),
        "-r".into(),
        base.into(),
    ];
    if let Some(conversation_name) = conversation_name {
        arguments.extend(["-m".into(), conversation_name.into()]);
    }
    arguments.push(target.into());
    arguments
}

fn verify_existing(repository: &Repository, target: &Path) -> Result<(), RuntimeError> {
    match repository.vcs {
        WorkspaceVcs::Git => {
            let source = git_common_dir(&repository.root)?;
            let target_common = git_common_dir(target)?;
            if source != target_common {
                return Err(RuntimeError::InvalidOption(format!(
                    "existing path is a worktree of a different repository: {}",
                    target.display()
                )));
            }
        }
        WorkspaceVcs::Jujutsu => {
            let source = jj_repo_store(&repository.root)?;
            let target_store = jj_repo_store(target)?;
            if source != target_store {
                return Err(RuntimeError::InvalidOption(format!(
                    "existing path is a workspace of a different Jujutsu repository: {}",
                    target.display()
                )));
            }
        }
    }
    Ok(())
}

fn binding_name(
    repository: &Repository,
    target: &Path,
    fallback: &str,
) -> Result<String, RuntimeError> {
    match repository.vcs {
        WorkspaceVcs::Git => {
            let output = command("git", &["branch", "--show-current"], target)?;
            let name = trim_output(&output)?;
            Ok(if name.is_empty() {
                fallback.to_owned()
            } else {
                name
            })
        }
        WorkspaceVcs::Jujutsu => Ok(fallback.to_owned()),
    }
}

fn git_common_dir(path: &Path) -> Result<PathBuf, RuntimeError> {
    let output = command("git", &["rev-parse", "--git-common-dir"], path)?;
    if !output.status.success() {
        return Err(command_error("git rev-parse --git-common-dir", &output));
    }
    let value = PathBuf::from(trim_output(&output)?);
    let value = if value.is_absolute() {
        value
    } else {
        path.join(value)
    };
    Ok(value.canonicalize()?)
}

fn jj_repo_store(path: &Path) -> Result<PathBuf, RuntimeError> {
    let mut root = path.canonicalize()?;
    while !root.join(".jj").exists() {
        if !root.pop() {
            return Err(RuntimeError::InvalidOption(
                "not a Jujutsu workspace".into(),
            ));
        }
    }
    let pointer = root.join(".jj").join("repo");
    if pointer.is_file() {
        let value = std::fs::read_to_string(&pointer)?;
        let value = PathBuf::from(value.trim());
        Ok(if value.is_absolute() {
            value
        } else {
            pointer.parent().unwrap().join(value)
        }
        .canonicalize()?)
    } else {
        Ok(pointer.canonicalize()?)
    }
}

fn copy_ignored(source: &Path, destination: &Path, mode: WorktreeCopy) -> Result<(), RuntimeError> {
    let include = build_ignore(source, ".worktreeinclude")?;
    let visible = ignore::WalkBuilder::new(source)
        .hidden(false)
        .ignore(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .path()
                .strip_prefix(source)
                .ok()
                .map(Path::to_path_buf)
        })
        .collect::<std::collections::HashSet<_>>();
    let walker = ignore::WalkBuilder::new(source)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .build();
    for entry in walker {
        let entry = entry.map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        let path = entry.path();
        let relative = path.strip_prefix(source).unwrap_or(path);
        if relative.as_os_str().is_empty() || excluded_copy_path(relative, destination, source) {
            continue;
        }
        let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        let is_ignored = !visible.contains(relative);
        let selected = is_ignored
            && (include
                .matched_path_or_any_parents(relative, is_dir)
                .is_ignore()
                || mode == WorktreeCopy::All);
        if !selected || is_dir {
            continue;
        }
        let target = destination.join(relative);
        if target.exists() || target.symlink_metadata().is_ok() {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            copy_symlink(path, &target)?;
        } else if metadata.is_file() {
            std::fs::copy(path, &target)?;
        }
    }
    Ok(())
}

fn build_ignore(root: &Path, file: &str) -> Result<Gitignore, RuntimeError> {
    let mut builder = GitignoreBuilder::new(root);
    let path = root.join(file);
    if path.is_file()
        && let Some(error) = builder.add(path)
    {
        return Err(RuntimeError::InvalidOption(error.to_string()));
    }
    builder
        .build()
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))
}

fn excluded_copy_path(relative: &Path, destination: &Path, source: &Path) -> bool {
    let mut components = relative.components();
    let first = components
        .next()
        .and_then(|component| component.as_os_str().to_str());
    if matches!(first, Some(".git" | ".jj")) {
        return true;
    }
    if relative.starts_with(Path::new(".cagent/worktrees")) {
        return true;
    }
    source.join(relative).starts_with(destination)
}

#[cfg(unix)]
fn copy_symlink(source: &Path, target: &Path) -> Result<(), RuntimeError> {
    std::os::unix::fs::symlink(std::fs::read_link(source)?, target)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(source: &Path, target: &Path) -> Result<(), RuntimeError> {
    let link = std::fs::read_link(source)?;
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(link, target)?;
    } else {
        std::os::windows::fs::symlink_file(link, target)?;
    }
    Ok(())
}

fn command(program: &str, arguments: &[&str], cwd: &Path) -> Result<Output, RuntimeError> {
    Command::new(program)
        .args(arguments)
        .current_dir(cwd)
        .output()
        .map_err(|error| RuntimeError::InvalidOption(format!("could not run {program}: {error}")))
}

fn ensure_command(program: &str, arguments: &[&str], cwd: &Path) -> Result<Output, RuntimeError> {
    let output = command(program, arguments, cwd)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(command_error(
            &std::iter::once(program)
                .chain(arguments.iter().copied())
                .collect::<Vec<_>>()
                .join(" "),
            &output,
        ))
    }
}

fn command_error(label: &str, output: &Output) -> RuntimeError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    RuntimeError::InvalidOption(if stderr.is_empty() {
        format!("{label} failed with {}", output.status)
    } else {
        format!("{label} failed: {stderr}")
    })
}

fn trim_output(output: &Output) -> Result<String, RuntimeError> {
    String::from_utf8(output.stdout.clone())
        .map(|value| value.trim().to_owned())
        .map_err(|error| {
            RuntimeError::InvalidOption(format!("command output was not UTF-8: {error}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(cwd: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn validates_portable_managed_names() {
        for valid in ["feature", "a.b-c_1"] {
            validate_name(valid).unwrap();
        }
        for invalid in ["", ".", "..", "a/b", "a\\b", "snowman-☃"] {
            assert!(validate_name(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn dot_directory_change_returns_to_the_launch_project() {
        let temporary = tempfile::tempdir().unwrap();
        let project = temporary.path().join("project");
        let external = temporary.path().join("external");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&external).unwrap();

        let transition = prepare_directory_change(&project, &external, Path::new(".")).unwrap();

        assert_eq!(transition.cwd, project.canonicalize().unwrap());
        assert_eq!(transition.previous_cwd, external);
    }

    #[test]
    fn worktree_configuration_defaults_and_copy_modes_parse() {
        let defaults =
            crate::ConfigSnapshot::parse(Path::new("config.toml"), "version = 1").unwrap();
        assert_eq!(defaults.worktree().base, "fresh");
        assert_eq!(defaults.worktree().branch_prefix, "worktree");
        assert_eq!(defaults.worktree().copy, WorktreeCopy::Off);

        let configured = crate::ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[worktree]\nbase = 'head'\nbranch_prefix = ''\ncopy = 'include'\n",
        )
        .unwrap();
        assert_eq!(configured.worktree().base, "head");
        assert_eq!(configured.worktree().branch_prefix, "");
        assert_eq!(configured.worktree().copy, WorktreeCopy::Include);
        assert!(
            crate::ConfigSnapshot::parse(
                Path::new("config.toml"),
                "version = 1\n[worktree]\ncopy = true\n",
            )
            .is_err()
        );
    }

    #[test]
    fn git_head_creation_and_existing_reentry_use_the_same_repository() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "cagent@example.invalid"]);
        git(&root, &["config", "user.name", "Cagent Test"]);
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        git(&root, &["add", "tracked.txt"]);
        git(&root, &["commit", "-qm", "initial"]);
        let config = WorktreeConfig {
            base: "head".into(),
            branch_prefix: "worktree".into(),
            copy: WorktreeCopy::Off,
        };
        let request = EnterWorktreeRequest {
            name: Some("topic".into()),
            path: None,
            base: None,
        };
        let nested_project = root.join("nested-project");
        std::fs::create_dir(&nested_project).unwrap();
        assert_eq!(
            prospective_worktree_path(&nested_project, &nested_project, &request).unwrap(),
            root.canonicalize().unwrap().join(".cagent/worktrees/topic")
        );
        let created = prepare_worktree_entry(&root, &root, &request, &config, None).unwrap();
        assert!(created.created);
        assert_eq!(
            created.branch_or_workspace.as_deref(),
            Some("worktree-topic")
        );
        assert!(created.cwd.join("tracked.txt").is_file());

        let entered = prepare_worktree_entry(&root, &root, &request, &config, None).unwrap();
        assert!(!entered.created);
        assert_eq!(entered.cwd, created.cwd);
    }

    #[test]
    fn jj_workspace_creation_uses_the_conversation_name_as_the_description() {
        assert_eq!(
            jj_workspace_add_arguments(
                "topic",
                "@",
                "/project/.cagent/worktrees/topic",
                Some("Fix the blue button"),
            ),
            [
                "workspace",
                "add",
                "--name",
                "topic",
                "-r",
                "@",
                "-m",
                "Fix the blue button",
                "/project/.cagent/worktrees/topic",
            ]
        );
        assert!(
            !jj_workspace_add_arguments("topic", "@", "/target", None)
                .iter()
                .any(|argument| argument == "-m")
        );
    }

    #[test]
    fn nested_git_repository_wins_over_parent_jujutsu_repository() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("parent");
        let project = parent.join("test-git");
        std::fs::create_dir_all(parent.join(".jj")).unwrap();
        std::fs::create_dir(&project).unwrap();
        git(&project, &["init", "-q"]);
        let request = EnterWorktreeRequest {
            name: Some("blue-button".into()),
            path: None,
            base: Some("head".into()),
        };

        let target = prospective_worktree_path(&project, &project, &request).unwrap();

        assert_eq!(
            target,
            project
                .canonicalize()
                .unwrap()
                .join(".cagent/worktrees/blue-button")
        );
        assert_eq!(
            display_worktree_target(&project, &project, &target),
            "blue-button"
        );
        assert_eq!(
            display_worktree_target(&project, &project, &project.join("worktrees/topic")),
            "worktrees/topic"
        );
    }

    #[test]
    fn project_paths_are_relative_to_the_previous_working_directory() {
        let project = Path::new("/project");
        let current = project.join("test");

        assert_eq!(display_directory_target(project, &current, project), "..");
        assert_eq!(
            display_directory_target(project, &current, &project.join("test2")),
            "../test2"
        );
        assert_eq!(
            display_worktree_target(project, &current, &project.join("custom/topic")),
            "../custom/topic"
        );
    }

    #[test]
    fn interactive_home_paths_expand_and_named_users_are_rejected() {
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();

        assert_eq!(expand_home_path(Path::new("~")).unwrap(), home);
        assert_eq!(
            expand_home_path(Path::new("~/project/src")).unwrap(),
            home.join("project/src")
        );
        assert!(matches!(
            expand_home_path(Path::new("~someone/project")),
            Err(RuntimeError::InvalidOption(message))
                if message.contains("named-user home paths are unsupported")
        ));
        assert_eq!(display_absolute_path(&home.join("project")), "~/project");
    }

    #[tokio::test]
    async fn session_directory_change_resolves_relative_paths_and_preserves_failures() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        let child = root.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(root.join("file.txt"), "not a directory").unwrap();
        let runtime = crate::runtime::AgentRuntime::open(crate::RuntimeOptions::new(
            temporary.path().join("data"),
        ))
        .await
        .unwrap();
        let session = runtime
            .create_session(crate::NewSession {
                workspace: root.clone(),
            })
            .await
            .unwrap();

        let transition = session.change_working_directory("child").await.unwrap();
        assert_eq!(transition.cwd, child.canonicalize().unwrap());
        assert_eq!(session.attach().await.unwrap().snapshot.cwd, transition.cwd);
        let error = session
            .change_working_directory("../file.txt")
            .await
            .unwrap_err();
        assert!(
            matches!(error, RuntimeError::InvalidOption(message) if message.contains("is not a directory"))
        );
        let snapshot = session.attach().await.unwrap().snapshot;
        assert_eq!(snapshot.cwd, transition.cwd);
        assert!(snapshot.transcript.iter().any(|block| matches!(
            &block.kind,
            crate::TranscriptBlockKind::WorkspaceTransition { label, target, .. }
                if label == "Change Working Directory" && target == "child"
        )));
    }

    #[tokio::test]
    async fn session_transition_persists_cwd_metadata_notice_and_project_scope() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "cagent@example.invalid"]);
        git(&root, &["config", "user.name", "Cagent Test"]);
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        git(&root, &["add", "tracked.txt"]);
        git(&root, &["commit", "-qm", "initial"]);

        let runtime = crate::runtime::AgentRuntime::open(crate::RuntimeOptions::new(
            temporary.path().join("data"),
        ))
        .await
        .unwrap();
        let session = runtime
            .create_session(crate::NewSession {
                workspace: root.clone(),
            })
            .await
            .unwrap();
        let transition = session
            .enter_worktree(EnterWorktreeRequest {
                name: Some("session".into()),
                path: None,
                base: Some("head".into()),
            })
            .await
            .unwrap();
        let snapshot = session.attach().await.unwrap().snapshot;
        assert_eq!(snapshot.project_dir, root.canonicalize().unwrap());
        assert_eq!(snapshot.cwd, transition.cwd);
        assert_eq!(snapshot.worktree, transition.worktree);
        assert!(snapshot.transcript.iter().any(|block| {
            matches!(
                &block.kind,
                crate::TranscriptBlockKind::WorkspaceTransition {
                    label,
                    target,
                    base,
                    ..
                } if label == "Enter Worktree" && target == "session" && base.is_none()
            )
        }));
        let conversations = runtime.conversations(Some(&root)).await.unwrap();
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].id, session.id());
        assert!(
            runtime
                .conversations(Some(&transition.cwd))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn continue_can_select_the_latest_conversation_in_a_named_worktree() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "cagent@example.invalid"]);
        git(&root, &["config", "user.name", "Cagent Test"]);
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        git(&root, &["add", "tracked.txt"]);
        git(&root, &["commit", "-qm", "initial"]);
        let runtime = crate::runtime::AgentRuntime::open(crate::RuntimeOptions::new(
            temporary.path().join("data"),
        ))
        .await
        .unwrap();
        let red = runtime
            .create_session_named(
                crate::NewSession {
                    workspace: root.clone(),
                },
                Some("Red conversation".into()),
            )
            .await
            .unwrap();
        red.enter_worktree(EnterWorktreeRequest {
            name: Some("red-button".into()),
            path: None,
            base: Some("head".into()),
        })
        .await
        .unwrap();
        let blue = runtime
            .create_session_named(
                crate::NewSession {
                    workspace: root.clone(),
                },
                Some("Blue conversation".into()),
            )
            .await
            .unwrap();
        blue.enter_worktree(EnterWorktreeRequest {
            name: Some("blue-button".into()),
            path: None,
            base: Some("head".into()),
        })
        .await
        .unwrap();

        let continued = runtime
            .continue_session_in_worktree(&root, "red-button")
            .await
            .unwrap();

        assert_eq!(continued.id(), red.id());
    }

    #[test]
    fn ignored_copy_respects_include_all_exclusions_and_no_overwrite() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(
            source.join(".gitignore"),
            ".env\nskip.env\n*.secret\nnested/\n",
        )
        .unwrap();
        std::fs::write(source.join(".worktreeinclude"), ".env\n!skip.env\n").unwrap();
        std::fs::write(source.join(".env"), "copied").unwrap();
        std::fs::write(source.join("skip.env"), "skip").unwrap();
        std::fs::write(source.join("token.secret"), "token").unwrap();
        std::fs::write(source.join("nested/value"), "nested").unwrap();
        std::fs::write(destination.join(".env"), "keep").unwrap();

        copy_ignored(&source, &destination, WorktreeCopy::Include).unwrap();
        assert_eq!(
            std::fs::read_to_string(destination.join(".env")).unwrap(),
            "keep"
        );
        assert!(!destination.join("token.secret").exists());
        assert!(!destination.join("skip.env").exists());

        copy_ignored(&source, &destination, WorktreeCopy::All).unwrap();
        assert_eq!(
            std::fs::read_to_string(destination.join("token.secret")).unwrap(),
            "token"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("nested/value")).unwrap(),
            "nested"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("skip.env")).unwrap(),
            "skip"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ignored_copy_preserves_symlinks_without_following_them() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join(".gitignore"), "linked.secret\n").unwrap();
        std::fs::write(source.join("actual"), "value").unwrap();
        std::os::unix::fs::symlink("actual", source.join("linked.secret")).unwrap();

        copy_ignored(&source, &destination, WorktreeCopy::All).unwrap();
        let copied = destination.join("linked.secret");
        assert!(
            std::fs::symlink_metadata(&copied)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(copied).unwrap(), PathBuf::from("actual"));
    }
}
