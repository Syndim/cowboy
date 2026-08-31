use std::path::{Component, Path, PathBuf};

use cowboy_workflow_core::{WorkflowSource, WorkflowSourceSnapshot};

use crate::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct SourceResolver;

impl SourceResolver {
    /// Load the entry and every dependency resolved while compiling it.
    pub fn load(&self, source: &WorkflowSource) -> Result<WorkflowSourceSnapshot> {
        crate::load(source).map(|compiled| compiled.source_bundle)
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
    use cowboy_workflow_core::WorkflowLocation;

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
        std::fs::write(
            &file,
            r#"
            local start = step("s")
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("x", start)
            "#,
        )
        .unwrap();
        let source = WorkflowSource {
            id: "x".into(),
            location: WorkflowLocation {
                root: Some(dir.path().to_path_buf()),
                import_roots: Vec::new(),
                entry: "main.lua".into(),
            },
            description: None,
        };
        let bundle = SourceResolver.load(&source).unwrap();
        assert!(bundle.files.contains_key("main.lua"));
    }
}
