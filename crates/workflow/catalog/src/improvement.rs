use std::path::Path;

use cowboy_workflow_core::{WorkflowCatalog, WorkflowId, WorkflowImprovement, WorkflowSourceRef};
use serde::{Deserialize, Serialize};

use crate::source::{load_source_ref, source_path_from_ref, write_source_ref};
use crate::{Error, Result};

/// A concrete workflow file write derived from an improvement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowSourceUpdate {
    UpdateExisting {
        workflow_id: WorkflowId,
        replacement_source: String,
    },
    CreateNew {
        draft: WorkflowSourceRef,
        replacement_source: String,
    },
}

/// Outcome of applying a workflow improvement to the catalog filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppliedWorkflowImprovement {
    None,
    Updated {
        source: WorkflowSourceRef,
        path: String,
    },
    Created {
        source: WorkflowSourceRef,
        path: String,
    },
}

pub fn apply_update(
    root: impl AsRef<Path>,
    catalog: &WorkflowCatalog,
    update: &WorkflowSourceUpdate,
) -> Result<AppliedWorkflowImprovement> {
    match update {
        WorkflowSourceUpdate::UpdateExisting {
            workflow_id,
            replacement_source,
        } => {
            let existing =
                catalog
                    .workflows
                    .get(workflow_id)
                    .ok_or_else(|| Error::UnknownWorkflow {
                        workflow_id: workflow_id.clone(),
                    })?;
            let loaded = write_source_ref(root.as_ref(), existing, replacement_source, true)?;
            let path = source_path_from_ref(&loaded.source_ref)?;
            Ok(AppliedWorkflowImprovement::Updated {
                path,
                source: loaded.source_ref,
            })
        }
        WorkflowSourceUpdate::CreateNew {
            draft,
            replacement_source,
        } => {
            if catalog.workflows.contains_key(&draft.id) {
                return Err(Error::AlreadyExists {
                    workflow_id: draft.id.clone(),
                    path: draft.entry.clone(),
                });
            }
            let loaded = write_source_ref(root.as_ref(), draft, replacement_source, false)?;
            let path = source_path_from_ref(&loaded.source_ref)?;
            Ok(AppliedWorkflowImprovement::Created {
                path,
                source: loaded.source_ref,
            })
        }
    }
}

pub fn apply_improvement(
    root: impl AsRef<Path>,
    catalog: &WorkflowCatalog,
    improvement: &WorkflowImprovement,
) -> Result<AppliedWorkflowImprovement> {
    match improvement {
        WorkflowImprovement::None { .. } => Ok(AppliedWorkflowImprovement::None),
        WorkflowImprovement::UpdateExisting {
            workflow_id, patch, ..
        } => {
            let replacement_source = patch.replacement_source.as_ref().ok_or_else(|| {
                Error::MissingReplacementSource {
                    workflow_id: workflow_id.clone(),
                }
            })?;
            apply_update(
                root,
                catalog,
                &WorkflowSourceUpdate::UpdateExisting {
                    workflow_id: workflow_id.clone(),
                    replacement_source: replacement_source.clone(),
                },
            )
        }
        WorkflowImprovement::CreateNew { draft, .. } => {
            let loaded = load_source_ref(draft)?;
            apply_update(
                root,
                catalog,
                &WorkflowSourceUpdate::CreateNew {
                    draft: draft.clone(),
                    replacement_source: loaded.source,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use cowboy_workflow_core::WorkflowPatch;

    use super::*;
    use crate::builtin::{DEFAULT_WORKFLOW_ENTRY, DEFAULT_WORKFLOW_ID, builtin_default_source_ref};

    fn catalog_with_default() -> WorkflowCatalog {
        WorkflowCatalog {
            workflows: BTreeMap::from([(
                DEFAULT_WORKFLOW_ID.to_string(),
                builtin_default_source_ref(),
            )]),
        }
    }

    #[test]
    fn applies_update_improvement_to_chosen_root() {
        let root = tempfile::tempdir().unwrap();
        let improvement = WorkflowImprovement::UpdateExisting {
            workflow_id: DEFAULT_WORKFLOW_ID.to_string(),
            patch: WorkflowPatch {
                description: "replace default".to_string(),
                replacement_source: Some("return workflow('default', step('start'))".to_string()),
            },
            rationale: "better workflow".to_string(),
        };

        let applied =
            apply_improvement(root.path(), &catalog_with_default(), &improvement).unwrap();
        let AppliedWorkflowImprovement::Updated { source, path } = applied else {
            panic!("expected update")
        };
        assert_eq!(source.id, DEFAULT_WORKFLOW_ID);
        assert_eq!(source.entry, DEFAULT_WORKFLOW_ENTRY);
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "return workflow('default', step('start'))"
        );
    }

    #[test]
    fn applies_create_improvement_from_draft_source_ref() {
        let draft_root = tempfile::tempdir().unwrap();
        let target_root = tempfile::tempdir().unwrap();
        fs::write(
            draft_root.path().join("custom.lua"),
            "return workflow('custom', step('start'))",
        )
        .unwrap();
        let draft = WorkflowSourceRef {
            id: "custom".to_string(),
            entry: "custom.lua".to_string(),
            root: Some(draft_root.path().to_string_lossy().to_string()),
            description: Some("custom workflow".to_string()),
        };
        let improvement = WorkflowImprovement::CreateNew {
            draft,
            rationale: "new workflow".to_string(),
        };

        let applied =
            apply_improvement(target_root.path(), &catalog_with_default(), &improvement).unwrap();
        let AppliedWorkflowImprovement::Created { source, path } = applied else {
            panic!("expected create")
        };
        assert_eq!(source.id, "custom");
        assert_eq!(source.entry, "custom.lua");
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "return workflow('custom', step('start'))"
        );
    }

    #[test]
    fn applies_create_update_to_chosen_root() {
        let root = tempfile::tempdir().unwrap();
        let draft = WorkflowSourceRef {
            id: "custom".to_string(),
            entry: "nested/custom.lua".to_string(),
            root: None,
            description: Some("custom workflow".to_string()),
        };
        let update = WorkflowSourceUpdate::CreateNew {
            draft,
            replacement_source: "return workflow('custom', step('start'))".to_string(),
        };

        let applied = apply_update(root.path(), &catalog_with_default(), &update).unwrap();
        let AppliedWorkflowImprovement::Created { source, path } = applied else {
            panic!("expected create")
        };
        assert_eq!(source.id, "custom");
        assert_eq!(source.entry, "nested/custom.lua");
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "return workflow('custom', step('start'))"
        );
    }
}
