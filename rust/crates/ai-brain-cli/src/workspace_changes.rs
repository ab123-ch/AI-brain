use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetectedChangeKind {
    Added,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetectedFileChange {
    pub path: PathBuf,
    pub kind: DetectedChangeKind,
}

#[derive(Debug)]
pub(crate) struct WorkspaceSnapshot {
    root: PathBuf,
    files: HashMap<PathBuf, String>,
}

impl WorkspaceSnapshot {
    pub(crate) fn capture(root: &Path) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        let candidate_paths = git_workspace_paths(&root).unwrap_or_else(|| {
            let mut paths = Vec::new();
            collect_workspace_paths(&root, &mut paths);
            paths
        });
        let mut files = HashMap::new();
        for candidate in candidate_paths {
            let Ok(metadata) = fs::symlink_metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let Ok(canonical) = fs::canonicalize(&candidate) else {
                continue;
            };
            if !canonical.starts_with(&root) {
                continue;
            }
            if let Ok(revision) = file_revision(&canonical) {
                files.insert(canonical, revision);
            }
        }
        Ok(Self { root, files })
    }

    pub(crate) fn changes_since(&self, current: &Self) -> Vec<DetectedFileChange> {
        if self.root != current.root {
            return Vec::new();
        }
        let mut changes = current
            .files
            .iter()
            .filter_map(|(path, revision)| match self.files.get(path) {
                None => Some(DetectedFileChange {
                    path: path.clone(),
                    kind: DetectedChangeKind::Added,
                }),
                Some(previous) if previous != revision => Some(DetectedFileChange {
                    path: path.clone(),
                    kind: DetectedChangeKind::Modified,
                }),
                Some(_) => None,
            })
            .collect::<Vec<_>>();
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        changes
    }
}

fn git_workspace_paths(root: &Path) -> Option<Vec<PathBuf>> {
    let output = Command::new("git")
        .args([
            "-C",
            root.to_str()?,
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|bytes| !bytes.is_empty())
            .filter_map(|bytes| std::str::from_utf8(bytes).ok())
            .map(|relative| root.join(relative))
            .collect(),
    )
}

fn collect_workspace_paths(directory: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if !is_generated_directory(&entry.file_name()) {
                collect_workspace_paths(&path, paths);
            }
        } else if file_type.is_file() {
            paths.push(path);
        }
    }
}

fn is_generated_directory(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git" | ".claw" | ".ai-brain" | ".clawd-agents" | "node_modules" | "target")
    )
}

fn file_revision(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_detects_added_and_modified_files_but_omits_deleted_files() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        fs::write(root.join("existing.txt"), "before").unwrap();
        fs::write(root.join("deleted.txt"), "delete me").unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/generated.txt"), "before").unwrap();
        let before = WorkspaceSnapshot::capture(root).unwrap();

        fs::write(root.join("existing.txt"), "after").unwrap();
        fs::remove_file(root.join("deleted.txt")).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/added.rs"), "fn added() {}\n").unwrap();
        fs::write(root.join("target/generated.txt"), "after").unwrap();
        let current = WorkspaceSnapshot::capture(root).unwrap();
        let canonical_root = fs::canonicalize(root).unwrap();

        let changes = before.changes_since(&current);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].kind, DetectedChangeKind::Modified);
        assert_eq!(changes[0].path, canonical_root.join("existing.txt"));
        assert_eq!(changes[1].kind, DetectedChangeKind::Added);
        assert_eq!(changes[1].path, canonical_root.join("nested/added.rs"));
    }

    #[test]
    fn unchanged_content_does_not_produce_a_change() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("same.txt"), "same").unwrap();
        let before = WorkspaceSnapshot::capture(temporary.path()).unwrap();
        fs::write(temporary.path().join("same.txt"), "same").unwrap();
        let current = WorkspaceSnapshot::capture(temporary.path()).unwrap();

        assert!(before.changes_since(&current).is_empty());
    }
}
