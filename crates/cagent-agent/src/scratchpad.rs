use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

const DIRECTORY_NAME: &str = "scratchpad";

pub(crate) fn configured_root(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os("CAGENT_TMPDIR").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
}

pub(crate) fn ensure(
    root: &Path,
    project_dir: &Path,
    conversation_id: crate::ConversationId,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    let user_root = root.join(user_directory_name());
    ensure_private_directory(&user_root)?;
    let project_root = user_root.join(project_key(project_dir));
    ensure_private_directory(&project_root)?;
    let conversation_root = project_root.join(conversation_id.to_string());
    ensure_private_directory(&conversation_root)?;
    let scratchpad = conversation_root.join(DIRECTORY_NAME);
    ensure_private_directory(&scratchpad)?;
    scratchpad.canonicalize()
}

pub(crate) fn remove_conversation(
    root: &Path,
    conversation_id: crate::ConversationId,
) -> std::io::Result<()> {
    let user_root = root.join(user_directory_name());
    let metadata = match std::fs::symlink_metadata(&user_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    validate_private_directory(&user_root, &metadata)?;
    for entry in std::fs::read_dir(&user_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let conversation = entry.path().join(conversation_id.to_string());
        let metadata = match std::fs::symlink_metadata(&conversation) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        validate_private_directory(&conversation, &metadata)?;
        std::fs::remove_dir_all(&conversation)?;
        let _ = std::fs::remove_dir(entry.path());
    }
    Ok(())
}

fn project_key(project_dir: &Path) -> String {
    let digest = Sha256::digest(project_dir.as_os_str().as_encoded_bytes());
    let mut key = String::with_capacity(16);
    for byte in &digest[..8] {
        write!(key, "{byte:02x}").expect("writing to a string cannot fail");
    }
    key
}

#[cfg(unix)]
fn user_directory_name() -> String {
    format!("cagent-{}", nix::unistd::Uid::effective().as_raw())
}

#[cfg(not(unix))]
fn user_directory_name() -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".into());
    let safe = user
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("cagent-{safe}")
}

fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    validate_private_directory(path, &metadata)?;
    make_private(path)
}

fn validate_private_directory(path: &Path, metadata: &std::fs::Metadata) -> std::io::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "scratchpad path is not a real directory: {}",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let expected = nix::unistd::Uid::effective().as_raw();
        if metadata.uid() != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "scratchpad path {} is owned by uid {}, expected {expected}",
                    path.display(),
                    metadata.uid()
                ),
            ));
        }
    }
    Ok(())
}

fn make_private(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_is_stable_and_isolated() {
        let temporary = tempfile::tempdir().unwrap();
        let project_a = temporary.path().join("a");
        let project_b = temporary.path().join("b");
        std::fs::create_dir(&project_a).unwrap();
        std::fs::create_dir(&project_b).unwrap();
        let first_id = crate::ConversationId::new();
        let second_id = crate::ConversationId::new();
        let first = ensure(temporary.path(), &project_a, first_id).unwrap();
        assert_eq!(
            first,
            ensure(temporary.path(), &project_a, first_id).unwrap()
        );
        assert_ne!(
            first,
            ensure(temporary.path(), &project_a, second_id).unwrap()
        );
        assert_ne!(
            first,
            ensure(temporary.path(), &project_b, first_id).unwrap()
        );
        assert!(first.ends_with(DIRECTORY_NAME));
    }

    #[test]
    fn deletion_is_idempotent() {
        let temporary = tempfile::tempdir().unwrap();
        let project = temporary.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let id = crate::ConversationId::new();
        let path = ensure(temporary.path(), &project, id).unwrap();
        remove_conversation(temporary.path(), id).unwrap();
        assert!(!path.exists());
        remove_conversation(temporary.path(), id).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn directories_are_private_and_symlinked_user_root_is_rejected() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let temporary = tempfile::tempdir().unwrap();
        let project = temporary.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let path = ensure(temporary.path(), &project, crate::ConversationId::new()).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let other = tempfile::tempdir().unwrap();
        let target = other.path().join("target");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, other.path().join(user_directory_name())).unwrap();
        let error = ensure(other.path(), &project, crate::ConversationId::new()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
