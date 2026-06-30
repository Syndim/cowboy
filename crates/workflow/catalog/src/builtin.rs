use cowboy_workflow_core::WorkflowSourceRef;

use crate::{Error, LoadedWorkflowSource, Result};

pub(crate) const DEFAULT_WORKFLOW_ID: &str = "default";
pub(crate) const DEFAULT_WORKFLOW_ENTRY: &str = "default.lua";
pub(crate) const DEFAULT_WORKFLOW_DESCRIPTION: &str = "Default developer implementation workflow";
pub(crate) const DEFAULT_WORKFLOW_SOURCE: &str = include_str!("workflows/default.lua");

pub fn builtin_default_workflow_source() -> LoadedWorkflowSource {
    LoadedWorkflowSource {
        source_ref: builtin_default_source_ref(),
        source: DEFAULT_WORKFLOW_SOURCE.to_string(),
    }
}

pub fn builtin_workflow_sources() -> Vec<LoadedWorkflowSource> {
    vec![builtin_default_workflow_source()]
}

pub fn builtin_default_source_ref() -> WorkflowSourceRef {
    WorkflowSourceRef {
        id: DEFAULT_WORKFLOW_ID.to_string(),
        entry: DEFAULT_WORKFLOW_ENTRY.to_string(),
        root: None,
        description: Some(DEFAULT_WORKFLOW_DESCRIPTION.to_string()),
    }
}

pub(crate) fn load_builtin_source_ref(
    source_ref: &WorkflowSourceRef,
) -> Result<LoadedWorkflowSource> {
    if source_ref.id == DEFAULT_WORKFLOW_ID && source_ref.entry == DEFAULT_WORKFLOW_ENTRY {
        return Ok(builtin_default_workflow_source());
    }
    Err(Error::UnknownBuiltin {
        workflow_id: source_ref.id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkflowCatalogLoader;

    #[test]
    fn builtin_catalog_entry_is_available() {
        let built_in = builtin_default_workflow_source();
        assert_eq!(built_in.source_ref.id, DEFAULT_WORKFLOW_ID);
        assert_eq!(built_in.source_ref.entry, DEFAULT_WORKFLOW_ENTRY);
        assert_eq!(built_in.source_ref.root, None);
        assert!(built_in.source.contains("role(\"developer\""));
        assert!(built_in.source.contains("action.agent"));
        assert!(built_in.source.contains("needs_fix"));

        let catalog = WorkflowCatalogLoader::new().load_catalog().unwrap();
        assert_eq!(
            catalog.workflows[DEFAULT_WORKFLOW_ID],
            builtin_default_source_ref()
        );
    }
}
