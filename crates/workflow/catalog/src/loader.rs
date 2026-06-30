use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use cowboy_workflow_core::{WorkflowCatalog, WorkflowId, WorkflowSourceRef};
use serde::{Deserialize, Serialize};

use crate::builtin::builtin_default_workflow_source;
use crate::source::{canonical_dir, io_error, load_source_ref, normalize_workflow_entry};
use crate::{LoadedWorkflowSource, Result};

/// Builder that assembles a workflow catalog from the built-in workflow and
/// configured filesystem roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowCatalogLoader {
    pub(crate) roots: Vec<CatalogRoot>,
    pub(crate) include_builtin: bool,
}

/// A filesystem directory scanned for `.lua` workflow files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRoot {
    pub path: String,
    pub kind: CatalogRootKind,
}

/// Origin of a catalog root, used for labeling and precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogRootKind {
    Project,
    User,
    Custom,
}

impl CatalogRoot {
    pub fn project(path: impl AsRef<Path>) -> Self {
        Self::new(path, CatalogRootKind::Project)
    }

    pub fn user(path: impl AsRef<Path>) -> Self {
        Self::new(path, CatalogRootKind::User)
    }

    pub fn custom(path: impl AsRef<Path>) -> Self {
        Self::new(path, CatalogRootKind::Custom)
    }

    pub fn new(path: impl AsRef<Path>, kind: CatalogRootKind) -> Self {
        Self {
            path: path.as_ref().to_string_lossy().to_string(),
            kind,
        }
    }
}

impl WorkflowCatalogLoader {
    pub fn new() -> Self {
        Self {
            roots: Vec::new(),
            include_builtin: true,
        }
    }

    pub fn without_builtin(mut self) -> Self {
        self.include_builtin = false;
        self
    }

    pub fn with_root(mut self, root: CatalogRoot) -> Self {
        self.roots.push(root);
        self
    }

    pub fn with_project_dir(self, path: impl AsRef<Path>) -> Self {
        self.with_root(CatalogRoot::project(path))
    }

    pub fn with_user_dir(self, path: impl AsRef<Path>) -> Self {
        self.with_root(CatalogRoot::user(path))
    }

    pub fn roots(&self) -> &[CatalogRoot] {
        &self.roots
    }

    pub fn load_catalog(&self) -> Result<WorkflowCatalog> {
        let workflows = self
            .load_sources()?
            .into_iter()
            .map(|source| (source.source_ref.id.clone(), source.source_ref))
            .collect::<BTreeMap<_, _>>();
        Ok(WorkflowCatalog { workflows })
    }

    pub fn load_sources(&self) -> Result<Vec<LoadedWorkflowSource>> {
        let mut sources = Vec::new();
        if self.include_builtin {
            sources.push(builtin_default_workflow_source());
        }
        for root in &self.roots {
            sources.extend(load_directory(root)?);
        }
        Ok(sources)
    }
}

impl Default for WorkflowCatalogLoader {
    fn default() -> Self {
        Self::new()
    }
}

fn load_directory(root: &CatalogRoot) -> Result<Vec<LoadedWorkflowSource>> {
    let root_path = Path::new(&root.path);
    if !root_path.exists() {
        return Ok(Vec::new());
    }
    let root_path = canonical_dir(root_path)?;
    let mut files = Vec::new();
    collect_lua_files(&root_path, &root_path, &mut files)?;
    files.sort();

    let mut sources = Vec::new();
    for entry in files {
        let source_ref = WorkflowSourceRef {
            id: workflow_id_from_entry(&entry),
            entry: entry.clone(),
            root: Some(root_path.to_string_lossy().to_string()),
            description: None,
        };
        let loaded = load_source_ref(&source_ref)?;
        match cowboy_workflow_lua::load(&loaded.source_ref) {
            Ok(_) => sources.push(loaded),
            Err(err) if is_non_workflow_source(&err) => {}
            Err(err) => {
                return Err(crate::Error::InvalidWorkflowSource {
                    workflow_id: source_ref.id,
                    message: err.to_string(),
                });
            }
        }
    }
    Ok(sources)
}
fn is_non_workflow_source(err: &cowboy_workflow_lua::Error) -> bool {
    match err {
        cowboy_workflow_lua::Error::MissingWorkflow => true,
        cowboy_workflow_lua::Error::UnsupportedValue(path) => path == "workflow",
        _ => false,
    }
}

fn collect_lua_files(root: &Path, dir: &Path, files: &mut Vec<String>) -> Result<()> {
    let mut entries = fs::read_dir(dir)
        .map_err(|err| io_error(dir, err))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| io_error(dir, err))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|err| io_error(&path, err))?;
        if file_type.is_dir() {
            collect_lua_files(root, &path, files)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| crate::Error::InvalidRelativePath(path.display().to_string()))?;
            let relative = relative
                .to_str()
                .ok_or_else(|| crate::Error::NonUtf8Path(path.display().to_string()))?;
            if relative.ends_with(".lua") {
                files.push(normalize_workflow_entry(relative)?);
            }
        }
    }
    Ok(())
}

fn workflow_id_from_entry(entry: &str) -> WorkflowId {
    entry.trim_end_matches(".lua").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_lua_file_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let workflows = dir.path().join("workflows");
        fs::create_dir(&workflows).unwrap();
        fs::write(
            workflows.join("review.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("review", start)
            "#,
        )
        .unwrap();
        fs::write(workflows.join("notes.txt"), "ignored").unwrap();

        let sources = WorkflowCatalogLoader::new()
            .without_builtin()
            .with_project_dir(dir.path())
            .load_sources()
            .unwrap();

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_ref.id, "workflows/review");
        assert_eq!(sources[0].source_ref.entry, "workflows/review.lua");
        assert!(sources[0].source.contains("return workflow"));
    }

    #[test]
    fn skips_lua_files_that_do_not_return_workflows() {
        let dir = tempfile::tempdir().unwrap();
        let roles = dir.path().join("roles");
        let workflows = dir.path().join("workflows");
        fs::create_dir(&roles).unwrap();
        fs::create_dir(&workflows).unwrap();
        fs::write(
            roles.join("planner.lua"),
            r#"return role("planner", "Plan work")"#,
        )
        .unwrap();
        fs::write(
            workflows.join("feature.lua"),
            r#"
            local planner = require("roles/planner.lua")
            local start = step("start", { role = planner })
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("feature", start, { description = "Feature work" })
            "#,
        )
        .unwrap();

        let catalog = WorkflowCatalogLoader::new()
            .without_builtin()
            .with_project_dir(dir.path())
            .load_catalog()
            .unwrap();

        assert!(catalog.workflows.contains_key("workflows/feature"));
        assert!(!catalog.workflows.contains_key("roles/planner"));
    }
}
