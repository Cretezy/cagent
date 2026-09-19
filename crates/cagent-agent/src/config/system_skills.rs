use std::path::{Path, PathBuf};

use include_dir::{Dir, DirEntry};
use sha2::{Digest as _, Sha256};

const BUNDLE: Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/config/system_skills");
const MARKER: &str = ".cagent-system-skills.marker";
const FINGERPRINT_SALT: &[u8] = b"cagent-system-skills-v1\0";

pub(super) fn root(config_dir: &Path) -> PathBuf {
    config_dir.join("skills/.system")
}

pub(super) fn install(config_dir: &Path) -> std::io::Result<PathBuf> {
    let destination = root(config_dir);
    let fingerprint = fingerprint();
    if destination.is_dir()
        && std::fs::read_to_string(destination.join(MARKER))
            .is_ok_and(|value| value.trim() == fingerprint)
    {
        return Ok(destination);
    }
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    write_dir(&BUNDLE, &destination)?;
    std::fs::write(destination.join(MARKER), format!("{fingerprint}\n"))?;
    Ok(destination)
}

fn fingerprint() -> String {
    let mut entries = Vec::new();
    collect_entries(&BUNDLE, &mut entries);
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    digest.update(FINGERPRINT_SALT);
    for (path, contents) in entries {
        digest.update(path.as_bytes());
        digest.update([0]);
        match contents {
            Some(contents) => {
                digest.update(b"file\0");
                digest.update(contents);
            }
            None => digest.update(b"dir\0"),
        }
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

fn collect_entries<'a>(dir: &'a Dir<'a>, entries: &mut Vec<(String, Option<&'a [u8]>)>) {
    for entry in dir.entries() {
        match entry {
            DirEntry::Dir(child) => {
                entries.push((child.path().to_string_lossy().into_owned(), None));
                collect_entries(child, entries);
            }
            DirEntry::File(file) => entries.push((
                file.path().to_string_lossy().into_owned(),
                Some(file.contents()),
            )),
        }
    }
}

fn write_dir(dir: &Dir<'_>, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in dir.entries() {
        match entry {
            DirEntry::Dir(child) => {
                std::fs::create_dir_all(destination.join(child.path()))?;
                write_dir(child, destination)?;
            }
            DirEntry::File(file) => {
                let path = destination.join(file.path());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, file.contents())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_reuses_and_replaces_bundle() {
        let temporary = tempfile::tempdir().unwrap();
        let root = install(temporary.path()).unwrap();
        let marker = std::fs::read_to_string(root.join(MARKER)).unwrap();
        assert_eq!(marker.trim(), fingerprint());
        std::fs::write(root.join("preserved"), "same fingerprint").unwrap();
        install(temporary.path()).unwrap();
        assert!(root.join("preserved").exists());
        std::fs::write(root.join(MARKER), "stale\n").unwrap();
        install(temporary.path()).unwrap();
        assert!(!root.join("preserved").exists());
        assert!(root.join("customize-cagent/SKILL.md").exists());
    }

    #[test]
    fn fingerprint_includes_nested_assets() {
        let mut entries = Vec::new();
        collect_entries(&BUNDLE, &mut entries);
        assert!(
            entries
                .iter()
                .any(|(path, _)| path == "skill-installer/scripts/list-skills.py")
        );
    }
}
