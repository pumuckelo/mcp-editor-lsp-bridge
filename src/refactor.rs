//! Bounded refactor snapshots and staged text edits. No language or transport policy.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
};

pub const MAX_EDIT_BYTES: usize = 16 * 1024 * 1024;
pub fn fingerprint(text: &[u8]) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hash);
    hash.finish()
}
#[derive(Clone, PartialEq, Eq)]
pub struct DocumentStamp {
    pub hash: u64,
    pub version: i64,
    pub epoch: u64,
}
#[derive(Clone)]
pub struct Snapshot {
    pub instance: String,
    pub disk: BTreeMap<PathBuf, u64>,
    pub documents: BTreeMap<String, DocumentStamp>,
}
// Only source/configuration files can affect a supported refactor. Skip dependency/build trees.
pub fn disk_snapshot(root: &Path) -> Result<BTreeMap<PathBuf, u64>> {
    let mut pending = vec![root.to_owned()];
    let mut files = BTreeMap::new();
    let mut bytes = 0;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("Read {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "node_modules" | "target" | "dist" | ".next")
                ) {
                    pending.push(path);
                }
            } else if kind.is_file() && relevant(&path) {
                let size = entry.metadata()?.len();
                ensure!(
                    size <= MAX_EDIT_BYTES as u64,
                    "Refactor snapshot file too large: {}",
                    path.display()
                );
                bytes += size;
                ensure!(
                    bytes <= 256 * 1024 * 1024 && files.len() < 100_000,
                    "Refactor snapshot exceeds workspace limit; choose a smaller workspace root"
                );
                files.insert(path, fingerprint(&std::fs::read(entry.path())?));
            }
        }
    }
    Ok(files)
}
fn relevant(path: &Path) -> bool {
    crate::language::Language::for_path(path).is_ok()
        || path.file_name().is_some_and(|name| {
            matches!(
                name.to_str(),
                Some(
                    "Cargo.toml"
                        | "Cargo.lock"
                        | "rust-toolchain.toml"
                        | "config.toml"
                        | "package.json"
                        | "jsconfig.json"
                        | "bun.lock"
                        | "package-lock.json"
                        | "pnpm-lock.yaml"
                        | "yarn.lock"
                )
            ) || name.to_string_lossy().starts_with("tsconfig")
        })
}
pub struct DocumentEdits {
    pub version: Option<i64>,
    pub edits: Vec<Value>,
}
pub fn text_edit_groups(edit: &Value) -> Result<BTreeMap<String, DocumentEdits>> {
    let mut groups = BTreeMap::new();
    if let Some(changes) = edit.get("changes") {
        for (uri, edits) in changes.as_object().context("Invalid changes map")? {
            groups.insert(
                uri.clone(),
                DocumentEdits {
                    version: None,
                    edits: edits.as_array().context("Invalid edits")?.clone(),
                },
            );
        }
    }
    if let Some(changes) = edit.get("documentChanges") {
        for change in changes.as_array().context("Invalid documentChanges")? {
            ensure!(
                change.get("kind").is_none(),
                "File create/rename/delete operations are unsupported; no edits applied"
            );
            let uri = change["textDocument"]["uri"]
                .as_str()
                .context("Missing document URI")?;
            ensure!(
                !groups.contains_key(uri),
                "Duplicate document edit group; no edits applied"
            );
            let version = match &change["textDocument"]["version"] {
                Value::Null => None,
                value => Some(value.as_i64().context("Invalid document version")?),
            };
            groups.insert(
                uri.into(),
                DocumentEdits {
                    version,
                    edits: change["edits"].as_array().context("Missing edits")?.clone(),
                },
            );
        }
    }
    ensure!(!groups.is_empty(), "Language server returned no text edits");
    Ok(groups)
}
pub struct FileEdit {
    pub path: PathBuf,
    pub original_disk: String,
    pub before: String,
    pub after: String,
    pub edits: Vec<Value>,
}
pub struct PreparedEdit {
    pub files: Vec<FileEdit>,
}
impl PreparedEdit {
    pub fn receipt(&self, root: &Path, applied: bool, verbose: bool) -> Value {
        let files: Vec<_> = self.files.iter().map(|file| {
            let mut lines: Vec<_> = file.edits.iter().filter_map(|e| e["range"]["start"]["line"].as_u64()).collect();
            lines.sort_unstable(); lines.dedup();
            let mut item = json!({"path":file.path.strip_prefix(root).unwrap_or(&file.path),"editCount":file.edits.len(),"lines":lines});
            if verbose {
                item["edits"] = json!(file.edits.iter().map(|e| {
                    let start = crate::protocol::offset(&file.before, &e["range"]["start"]).unwrap();
                    let end = crate::protocol::offset(&file.before, &e["range"]["end"]).unwrap();
                    json!({"range":e["range"],"oldText":&file.before[start..end],"newText":e["newText"]})
                }).collect::<Vec<_>>());
            }
            item
        }).collect();
        json!({"applied":applied,"fileCount":files.len(),"editCount":self.files.iter().map(|f| f.edits.len()).sum::<usize>(),"files":files})
    }
    pub fn commit(&self) -> Result<()> {
        // Stage all replacements first; no truncation and no writes to targets on staging failure.
        let mut staged = Staged(vec![]);
        for file in &self.files {
            ensure!(
                std::fs::read_to_string(&file.path)? == file.original_disk,
                "Stale refactor: {} changed; no edits applied",
                file.path.display()
            );
            let temp = file
                .path
                .with_file_name(format!(".bridge-edit-{}", uuid::Uuid::new_v4()));
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            staged.0.push(temp.clone());
            output.set_permissions(std::fs::metadata(&file.path)?.permissions())?;
            output.write_all(file.after.as_bytes())?;
        }
        for file in &self.files {
            ensure!(
                std::fs::read_to_string(&file.path)? == file.original_disk,
                "Stale refactor: {} changed during staging; no edits applied",
                file.path.display()
            );
        }
        for (index, file) in self.files.iter().enumerate() {
            if let Err(error) = std::fs::rename(&staged.0[index], &file.path) {
                let changed: Vec<_> = self.files[..index]
                    .iter()
                    .map(|f| f.path.display().to_string())
                    .collect();
                bail!(
                    "Refactor write failed at {}: {error}; already applied files: {changed:?}. Inspect disk before retrying",
                    file.path.display()
                );
            }
        }
        Ok(())
    }
}
struct Staged(Vec<PathBuf>);
impl Drop for Staged {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_resources_duplicates_and_stale_disk_before_writes() -> Result<()> {
        assert!(
            text_edit_groups(
                &json!({"documentChanges":[{"kind":"rename","oldUri":"a","newUri":"b"}]})
            )
            .is_err()
        );
        assert!(text_edit_groups(&json!({"changes":{"a":[]},"documentChanges":[{"textDocument":{"uri":"a","version":1},"edits":[]}]})).is_err());
        let temp = tempfile::tempdir()?;
        let a = temp.path().join("a.ts");
        let b = temp.path().join("b.ts");
        std::fs::write(&a, "before")?;
        std::fs::write(&b, "concurrent")?;
        let edit = PreparedEdit {
            files: vec![
                FileEdit {
                    path: a.clone(),
                    original_disk: "before".into(),
                    before: "before".into(),
                    after: "after".into(),
                    edits: vec![],
                },
                FileEdit {
                    path: b.clone(),
                    original_disk: "before".into(),
                    before: "before".into(),
                    after: "after".into(),
                    edits: vec![],
                },
            ],
        };
        assert!(edit.commit().is_err());
        assert_eq!(std::fs::read_to_string(a)?, "before");
        assert_eq!(std::fs::read_to_string(b)?, "concurrent");
        assert_eq!(std::fs::read_dir(temp.path())?.count(), 2);
        Ok(())
    }
}
