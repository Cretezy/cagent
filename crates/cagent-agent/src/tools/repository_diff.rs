//! Frontend-neutral loading and parsing of the current repository diff.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{DiffFile, DiffFileKind, DiffHunk, DiffLine, DiffLineKind, SemanticDiff};

const MAX_DIFF_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryKind {
    Jujutsu,
    Git,
}

impl RepositoryKind {
    fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Jujutsu => ("jj", &["diff", "--git"]),
            Self::Git => ("git", &["diff"]),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Jujutsu => "jj",
            Self::Git => "git",
        }
    }
}

#[derive(Debug, Error)]
pub enum RepositoryDiffError {
    #[error("not inside a Git or Jujutsu repository")]
    Unsupported,
    #[error("{vcs} diff failed: {message}")]
    Command { vcs: &'static str, message: String },
    #[error("repository diff is too large to display")]
    TooLarge,
}

/// Finds a repository containing `workspace`. Jujutsu wins when both kinds
/// are present because colocated Jujutsu repositories also contain `.git`.
pub fn repository_for_workspace(workspace: &Path) -> Option<(RepositoryKind, PathBuf)> {
    let mut git = None;
    for ancestor in workspace.ancestors() {
        if ancestor.join(".jj").exists() {
            return Some((RepositoryKind::Jujutsu, ancestor.to_path_buf()));
        }
        if git.is_none() && ancestor.join(".git").exists() {
            git = Some(ancestor.to_path_buf());
        }
    }
    git.map(|root| (RepositoryKind::Git, root))
}

/// Loads the working-copy diff using the repository's native command and
/// projects Git-format output into the shared semantic diff model.
pub async fn load_repository_diff(workspace: &Path) -> Result<SemanticDiff, RepositoryDiffError> {
    let (kind, root) =
        repository_for_workspace(workspace).ok_or(RepositoryDiffError::Unsupported)?;
    let (program, arguments) = kind.command();
    let mut child = tokio::process::Command::new(program)
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| RepositoryDiffError::Command {
            vcs: kind.label(),
            message: error.to_string(),
        })?;
    let stdout = child.stdout.take().expect("piped diff stdout");
    let stderr = child.stderr.take().expect("piped diff stderr");
    let (stdout, stderr, status) = tokio::try_join!(
        read_bounded_diff_output(stdout, MAX_DIFF_BYTES, kind),
        read_bounded_diff_output(stderr, 64 * 1024, kind),
        async {
            child
                .wait()
                .await
                .map_err(|error| RepositoryDiffError::Command {
                    vcs: kind.label(),
                    message: error.to_string(),
                })
        },
    )?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let message = stderr.lines().find(|line| !line.trim().is_empty());
        return Err(RepositoryDiffError::Command {
            vcs: kind.label(),
            message: if let Some(message) = message {
                message.trim().chars().take(240).collect()
            } else {
                format!("exited with {status}")
            },
        });
    }
    Ok(parse_git_diff(&String::from_utf8_lossy(&stdout)))
}

// Stop reading at the bound, not after collecting arbitrary subprocess output.
// An early error drops the other readers/wait future and kills the child.
async fn read_bounded_diff_output(
    reader: impl AsyncRead + Unpin,
    limit: usize,
    kind: RepositoryKind,
) -> Result<Vec<u8>, RepositoryDiffError> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| RepositoryDiffError::Command {
            vcs: kind.label(),
            message: error.to_string(),
        })?;
    if bytes.len() > limit {
        return Err(RepositoryDiffError::TooLarge);
    }
    Ok(bytes)
}

/// Parses the stable Git patch format emitted by both `git diff` and
/// `jj diff --git`.
pub fn parse_git_diff(source: &str) -> SemanticDiff {
    let mut files = Vec::new();
    let mut current = None::<DiffFile>;
    let mut current_hunk = None::<DiffHunk>;
    let mut old_line = 0_u64;
    let mut new_line = 0_u64;

    let finish_hunk = |file: &mut Option<DiffFile>, hunk: &mut Option<DiffHunk>| {
        if let (Some(file), Some(hunk)) = (file.as_mut(), hunk.take()) {
            file.hunks.push(hunk);
        }
    };
    let finish_file = |files: &mut Vec<DiffFile>, file: &mut Option<DiffFile>| {
        if let Some(mut file) = file.take() {
            let path = file.new_path.as_deref().or(file.old_path.as_deref());
            file.language =
                path.and_then(|path| crate::presentation::syntax_language_for_path(path, ""));
            files.push(file);
        }
    };

    for line in source.lines() {
        if let Some(paths) = line.strip_prefix("diff --git ") {
            finish_hunk(&mut current, &mut current_hunk);
            finish_file(&mut files, &mut current);
            let (old_path, new_path) = parse_git_path_pair(paths).unwrap_or_default();
            current = Some(DiffFile {
                old_path: old_path.and_then(|path| strip_diff_prefix(path, "a/")),
                new_path: new_path.and_then(|path| strip_diff_prefix(path, "b/")),
                kind: DiffFileKind::Modified,
                language: None,
                added_lines: 0,
                removed_lines: 0,
                old_no_final_newline: false,
                new_no_final_newline: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = current.as_mut() else {
            continue;
        };
        if line.starts_with("new file mode ") {
            file.kind = DiffFileKind::Added;
            file.old_path = None;
        } else if line.starts_with("deleted file mode ") {
            file.kind = DiffFileKind::Deleted;
            file.new_path = None;
        } else if let Some(path) = line.strip_prefix("rename from ") {
            file.kind = DiffFileKind::Renamed;
            file.old_path = Some(PathBuf::from(parse_git_path(path)));
        } else if let Some(path) = line.strip_prefix("rename to ") {
            file.kind = DiffFileKind::Renamed;
            file.new_path = Some(PathBuf::from(parse_git_path(path)));
        } else if let Some(path) = line.strip_prefix("--- ") {
            file.old_path = parse_header_path(path, "a/");
        } else if let Some(path) = line.strip_prefix("+++ ") {
            file.new_path = parse_header_path(path, "b/");
        } else if line.starts_with("@@ ") {
            finish_hunk(&mut current, &mut current_hunk);
            let (old, new) = parse_hunk_starts(line).unwrap_or((0, 0));
            old_line = old;
            new_line = new;
            current_hunk = Some(DiffHunk {
                header: line.to_owned(),
                lines: Vec::new(),
            });
        } else if let Some(hunk) = current_hunk.as_mut() {
            let parsed = match line.as_bytes().first() {
                Some(b' ') => {
                    let parsed = DiffLine {
                        kind: DiffLineKind::Context,
                        old_line: Some(old_line),
                        new_line: Some(new_line),
                        text: line[1..].to_owned(),
                    };
                    old_line += 1;
                    new_line += 1;
                    Some(parsed)
                }
                Some(b'-') => {
                    file.removed_lines += 1;
                    let parsed = DiffLine {
                        kind: DiffLineKind::Deletion,
                        old_line: Some(old_line),
                        new_line: None,
                        text: line[1..].to_owned(),
                    };
                    old_line += 1;
                    Some(parsed)
                }
                Some(b'+') => {
                    file.added_lines += 1;
                    let parsed = DiffLine {
                        kind: DiffLineKind::Addition,
                        old_line: None,
                        new_line: Some(new_line),
                        text: line[1..].to_owned(),
                    };
                    new_line += 1;
                    Some(parsed)
                }
                _ => None,
            };
            if let Some(parsed) = parsed {
                hunk.lines.push(parsed);
            } else if line == "\\ No newline at end of file"
                && let Some(previous) = hunk.lines.last()
            {
                match previous.kind {
                    DiffLineKind::Deletion => file.old_no_final_newline = true,
                    DiffLineKind::Addition => file.new_no_final_newline = true,
                    DiffLineKind::Context => {
                        file.old_no_final_newline = true;
                        file.new_no_final_newline = true;
                    }
                }
            }
        }
    }
    finish_hunk(&mut current, &mut current_hunk);
    finish_file(&mut files, &mut current);
    SemanticDiff { files }
}

fn parse_hunk_starts(header: &str) -> Option<(u64, u64)> {
    let mut parts = header.split_whitespace();
    (parts.next()? == "@@").then_some(())?;
    let old = parts
        .next()?
        .strip_prefix('-')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    let new = parts
        .next()?
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

fn parse_header_path(value: &str, prefix: &str) -> Option<PathBuf> {
    let value = value.split('\t').next().unwrap_or(value);
    (value != "/dev/null")
        .then(|| parse_git_path(value))
        .and_then(|path| strip_diff_prefix(path, prefix))
}

fn strip_diff_prefix(path: String, prefix: &str) -> Option<PathBuf> {
    Some(PathBuf::from(path.strip_prefix(prefix).unwrap_or(&path)))
}

fn parse_git_path_pair(value: &str) -> Option<(Option<String>, Option<String>)> {
    let mut paths = Vec::with_capacity(2);
    let mut rest = value.trim_start();
    while paths.len() < 2 && !rest.is_empty() {
        let (token, tail) = take_git_token(rest)?;
        paths.push(parse_git_path(token));
        rest = tail.trim_start();
    }
    (paths.len() == 2).then(|| (Some(paths.remove(0)), Some(paths.remove(0))))
}

fn take_git_token(value: &str) -> Option<(&str, &str)> {
    if value.starts_with('"') {
        let mut escaped = false;
        for (index, character) in value.char_indices().skip(1) {
            if character == '"' && !escaped {
                return Some((&value[..=index], &value[index + 1..]));
            }
            escaped = character == '\\' && !escaped;
            if character != '\\' {
                escaped = false;
            }
        }
        None
    } else {
        let end = value.find(char::is_whitespace).unwrap_or(value.len());
        Some((&value[..end], &value[end..]))
    }
}

fn parse_git_path(value: &str) -> String {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return value.to_owned();
    };
    let mut output = Vec::with_capacity(inner.len());
    let bytes = inner.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        match bytes.get(index).copied() {
            Some(b'n') => {
                output.push(b'\n');
                index += 1;
            }
            Some(b't') => {
                output.push(b'\t');
                index += 1;
            }
            Some(b'r') => {
                output.push(b'\r');
                index += 1;
            }
            Some(b'"' | b'\\') => {
                output.push(bytes[index]);
                index += 1;
            }
            Some(byte @ b'0'..=b'7') => {
                let mut value = byte - b'0';
                index += 1;
                for _ in 0..2 {
                    let Some(byte @ b'0'..=b'7') = bytes.get(index).copied() else {
                        break;
                    };
                    value = value.saturating_mul(8).saturating_add(byte - b'0');
                    index += 1;
                }
                output.push(value);
            }
            Some(byte) => {
                output.push(byte);
                index += 1;
            }
            None => output.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repository_diff_output_is_bounded_and_accepts_empty_or_exact_limit() {
        for source in [b"".as_slice(), b"1234".as_slice()] {
            assert_eq!(
                read_bounded_diff_output(source, 4, RepositoryKind::Git)
                    .await
                    .unwrap(),
                source
            );
        }
        assert!(matches!(
            read_bounded_diff_output(b"12345".as_slice(), 4, RepositoryKind::Jujutsu).await,
            Err(RepositoryDiffError::TooLarge)
        ));
        assert_eq!(
            RepositoryKind::Jujutsu.command(),
            ("jj", &["diff", "--git"][..])
        );
        assert_eq!(RepositoryKind::Git.command(), ("git", &["diff"][..]));
    }

    #[test]
    fn parses_modified_added_deleted_renamed_and_multiple_hunks() {
        let source = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
 fn main() {}
-old
+new
@@ -8 +8 @@
-before
+after
diff --git a/new.txt b/new.txt
new file mode 100644
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+new
\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-gone
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
"#;
        let diff = parse_git_diff(source);
        assert_eq!(diff.files.len(), 4);
        assert_eq!(diff.files[0].hunks.len(), 2);
        assert_eq!(
            (diff.files[0].added_lines, diff.files[0].removed_lines),
            (2, 2)
        );
        assert_eq!(diff.files[1].kind, DiffFileKind::Added);
        assert!(diff.files[1].new_no_final_newline);
        assert_eq!(diff.files[2].kind, DiffFileKind::Deleted);
        assert_eq!(diff.files[3].kind, DiffFileKind::Renamed);
        assert_eq!(
            diff.files[3].new_path.as_deref(),
            Some(Path::new("new name.txt"))
        );
    }

    #[test]
    fn parses_git_quoted_paths() {
        let diff = parse_git_diff("diff --git \"a/a b\\t.txt\" \"b/a b\\t.txt\"\n");
        assert_eq!(
            diff.files[0].old_path.as_deref(),
            Some(Path::new("a b\t.txt"))
        );
    }

    #[test]
    fn repository_detection_prefers_jj_and_walks_ancestors() {
        let temporary = tempfile::tempdir().unwrap();
        let nested = temporary.path().join("one/two");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(temporary.path().join(".git")).unwrap();
        assert_eq!(
            repository_for_workspace(&nested).unwrap().0,
            RepositoryKind::Git
        );
        assert_eq!(RepositoryKind::Git.command(), ("git", &["diff"][..]));
        std::fs::create_dir(temporary.path().join(".jj")).unwrap();
        assert_eq!(
            repository_for_workspace(&nested).unwrap().0,
            RepositoryKind::Jujutsu
        );
        assert_eq!(
            RepositoryKind::Jujutsu.command(),
            ("jj", &["diff", "--git"][..])
        );
    }
}
