use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use cowboy_workflow_core::{WorkflowSourceRef, WorkflowSourceSnapshot};

use crate::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct SourceResolver;

impl SourceResolver {
    pub fn load(&self, source: &WorkflowSourceRef) -> Result<WorkflowSourceSnapshot> {
        let root = source.root.as_ref().ok_or(Error::MissingRoot)?;
        let root_path = PathBuf::from(root);
        let canonical_root = root_path.canonicalize()?;
        let mut files = BTreeMap::new();
        let mut loading = BTreeSet::new();
        self.load_one(&canonical_root, &source.entry, &mut files, &mut loading)?;
        Ok(WorkflowSourceSnapshot {
            root: Some(canonical_root.to_string_lossy().to_string()),
            entry: normalize_relative_path(&source.entry)?,
            files,
        })
    }

    fn load_one(
        &self,
        root: &Path,
        relative: &str,
        files: &mut BTreeMap<String, String>,
        loading: &mut BTreeSet<String>,
    ) -> Result<()> {
        let normalized = normalize_relative_path(relative)?;
        if files.contains_key(&normalized) || !loading.insert(normalized.clone()) {
            return Ok(());
        }
        let path = root.join(&normalized);
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(root) {
            return Err(Error::ImportOutsideRoot(relative.to_string()));
        }
        let source = std::fs::read_to_string(&canonical)?;
        files.insert(normalized.clone(), source);
        loading.remove(&normalized);
        Ok(())
    }
}

pub fn normalize_relative_path(path: &str) -> Result<String> {
    if path.trim().is_empty() {
        return Err(Error::EmptyImport);
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(Error::ImportOutsideRoot(path.display().to_string()));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::ImportOutsideRoot(path.display().to_string()));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(Error::EmptyImport);
    }
    Ok(normalized.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_dir_imports() {
        assert!(matches!(
            normalize_relative_path("../secrets.lua"),
            Err(Error::ImportOutsideRoot(_))
        ));
    }

    #[test]
    fn normalizes_current_dir_segments() {
        assert_eq!(
            normalize_relative_path("./roles/dev.lua").unwrap(),
            "roles/dev.lua"
        );
    }

    #[test]
    fn loads_entry_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.lua");
        std::fs::write(&file, "return workflow('x', step('s'))").unwrap();
        let source = WorkflowSourceRef {
            id: "x".into(),
            entry: "main.lua".into(),
            root: Some(dir.path().to_string_lossy().to_string()),
            description: None,
        };
        let bundle = SourceResolver.load(&source).unwrap();
        assert!(bundle.files.contains_key("main.lua"));
    }
}
