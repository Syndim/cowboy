use std::fs;
use std::path::{Component, Path, PathBuf};

use cowboy_workflow_core::WorkflowSourceRef;
use serde::{Deserialize, Serialize};

use crate::builtin::load_builtin_source_ref;
use crate::{Error, Result};

/// A workflow source ref paired with its loaded Lua source text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedWorkflowSource {
    pub source_ref: WorkflowSourceRef,
    pub source: String,
}

pub fn load_source_ref(source_ref: &WorkflowSourceRef) -> Result<LoadedWorkflowSource> {
    if source_ref.root.is_none() {
        return load_builtin_source_ref(source_ref);
    }

    let root = source_ref.root.as_ref().ok_or_else(|| Error::MissingRoot {
        workflow_id: source_ref.id.clone(),
    })?;
    let entry = normalize_workflow_entry(&source_ref.entry)?;
    let root_path = canonical_dir(Path::new(root))?;
    let path = root_path.join(&entry);
    let canonical = canonical_file(&path)?;
    if !canonical.starts_with(&root_path) {
        return Err(Error::InvalidRelativePath(source_ref.entry.clone()));
    }
    let source = read_to_string(&canonical)?;
    Ok(LoadedWorkflowSource {
        source_ref: WorkflowSourceRef {
            id: source_ref.id.clone(),
            entry,
            root: Some(root_path.to_string_lossy().to_string()),
            description: source_ref.description.clone(),
        },
        source,
    })
}

pub fn materialize_source_ref(
    root: impl AsRef<Path>,
    source_ref: &WorkflowSourceRef,
    replacement_source: &str,
) -> Result<WorkflowSourceRef> {
    write_source_ref(root.as_ref(), source_ref, replacement_source, true)
        .map(|loaded| loaded.source_ref)
}

pub(crate) fn write_source_ref(
    root: &Path,
    source_ref: &WorkflowSourceRef,
    replacement_source: &str,
    overwrite: bool,
) -> Result<LoadedWorkflowSource> {
    if source_ref.id.trim().is_empty() {
        return Err(Error::EmptyWorkflowId);
    }
    let entry = normalize_workflow_entry(&source_ref.entry)?;
    fs::create_dir_all(root).map_err(|err| io_error(root, err))?;
    let root = canonical_dir(root)?;
    let path = source_path(&root, &entry)?;
    if !overwrite && path.exists() {
        return Err(Error::AlreadyExists {
            workflow_id: source_ref.id.clone(),
            path: path.to_string_lossy().to_string(),
        });
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
    }
    fs::write(&path, replacement_source).map_err(|err| io_error(&path, err))?;
    Ok(LoadedWorkflowSource {
        source_ref: WorkflowSourceRef {
            id: source_ref.id.clone(),
            entry,
            root: Some(root.to_string_lossy().to_string()),
            description: source_ref.description.clone(),
        },
        source: replacement_source.to_string(),
    })
}

/// Validate and normalize a workflow entry path to a safe relative `.lua` path.
pub fn normalize_workflow_entry(path: &str) -> Result<String> {
    if path.trim().is_empty() {
        return Err(Error::InvalidRelativePath(path.to_string()));
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(Error::InvalidRelativePath(path.display().to_string()));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::InvalidRelativePath(path.display().to_string()));
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(Error::InvalidRelativePath(path.display().to_string()));
    }
    let normalized = normalized
        .to_str()
        .ok_or_else(|| Error::NonUtf8Path(path.display().to_string()))?
        .replace('\\', "/");
    if !normalized.ends_with(".lua") {
        return Err(Error::NonLuaEntry(normalized));
    }
    Ok(normalized)
}

pub(crate) fn source_path(root: &Path, entry: &str) -> Result<PathBuf> {
    let entry = normalize_workflow_entry(entry)?;
    Ok(root.join(entry))
}

pub(crate) fn source_path_from_ref(source_ref: &WorkflowSourceRef) -> Result<String> {
    let root = source_ref.root.as_ref().ok_or_else(|| Error::MissingRoot {
        workflow_id: source_ref.id.clone(),
    })?;
    Ok(source_path(Path::new(root), &source_ref.entry)?
        .to_string_lossy()
        .to_string())
}

pub(crate) fn canonical_dir(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|err| io_error(path, err))?;
    if !canonical.is_dir() {
        return Err(Error::NotDirectory(canonical.to_string_lossy().to_string()));
    }
    Ok(canonical)
}

pub(crate) fn canonical_file(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|err| io_error(path, err))?;
    if !canonical.is_file() {
        return Err(Error::InvalidRelativePath(
            canonical.to_string_lossy().to_string(),
        ));
    }
    Ok(canonical)
}

pub(crate) fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|err| io_error(path, err))
}

pub(crate) fn io_error(path: &Path, err: std::io::Error) -> Error {
    Error::Io {
        path: path.to_string_lossy().to_string(),
        message: err.to_string(),
    }
}
