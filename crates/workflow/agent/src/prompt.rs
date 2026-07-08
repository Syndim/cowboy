use cowboy_workflow_core::{AgentAction, OutputSpec, RoleDefinition};
use serde_json::Value;

pub fn build_agent_prompt(role: &RoleDefinition, action: &AgentAction) -> String {
    let mut parts = Vec::new();
    if !role.instructions.trim().is_empty() {
        parts.push(format!("## Role\n\n{}", role.instructions.trim()));
    }
    parts.push(format!("## Task\n\n{}", action.prompt.trim()));
    if let Some(output) = &action.output {
        parts.push(build_output_instruction(output));
    }
    parts.join("\n\n")
}

/// Build a corrective instruction appended to a retry prompt after a
/// frontmatter/parse failure. Reuses the required-frontmatter description so the
/// agent re-emits its already-completed work with a valid frontmatter block.
pub fn build_retry_nudge(action: &AgentAction, reason: Option<&str>) -> String {
    let mut nudge =
        String::from("## Retry\n\nYour previous response could not be parsed as a workflow result");
    if let Some(reason) = reason {
        nudge.push_str(&format!(" ({reason})"));
    }
    nudge.push_str(
        ".\n\nDo not redo the work. Re-emit your result now, and make sure the response \
BEGINS with a valid YAML frontmatter block containing a `status` field.",
    );
    if let Some(output) = &action.output {
        nudge.push_str("\n\n");
        nudge.push_str(&build_output_instruction(output));
    }
    nudge
}

fn build_output_instruction(output: &OutputSpec) -> String {
    let statuses = if output.statuses.is_empty() {
        "Any status required by the workflow.".to_string()
    } else {
        output.statuses.join(", ")
    };
    let fields = describe_fields(&output.fields);
    format!(
        "## Deliverable Format\n\n\
Your response MUST begin with valid YAML frontmatter followed by Markdown body. Quote frontmatter strings and list items that contain `: `, backticks, brackets, braces, or other YAML punctuation.\n\n\
Allowed status values: {statuses}\n\n\
Frontmatter fields:\n{fields}\n\n\
Example:\n\n---\nstatus: success\nsummary: short summary\n---\n\nMarkdown details here."
    )
}

fn describe_fields(fields: &Value) -> String {
    let Some(object) = fields.as_object() else {
        return "- status: routing status string".to_string();
    };
    let mut lines = vec!["- status: routing status string".to_string()];
    for (key, value) in object {
        lines.push(format!("- {key}: {}", field_description(value)));
    }
    lines.join("\n")
}

fn field_description(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_includes_role_task_and_frontmatter_instruction() {
        let role = RoleDefinition {
            id: "dev".into(),
            instructions: "Implement changes".into(),
            agent: None,
            properties: Value::Null,
        };
        let action = AgentAction {
            role: "dev".into(),
            prompt: "Do work".into(),
            output: Some(OutputSpec {
                statuses: vec!["success".into(), "failed".into()],
                fields: serde_json::json!({"summary": "string"}),
            }),
        };
        let prompt = build_agent_prompt(&role, &action);
        assert!(prompt.contains("Implement changes"));
        assert!(prompt.contains("Do work"));
        assert!(prompt.contains("valid YAML frontmatter"));
        assert!(prompt.contains("success, failed"));
        assert!(prompt.contains("summary"));
    }

    #[test]
    fn retry_nudge_includes_reason_and_frontmatter_instruction() {
        let action = AgentAction {
            role: "dev".into(),
            prompt: "Do work".into(),
            output: Some(OutputSpec {
                statuses: vec!["success".into()],
                fields: serde_json::json!({"summary": "string"}),
            }),
        };
        let nudge = build_retry_nudge(&action, Some("missing YAML frontmatter"));
        assert!(nudge.contains("Retry"));
        assert!(nudge.contains("missing YAML frontmatter"));
        assert!(nudge.contains("YAML frontmatter"));
        assert!(nudge.contains("status"));
        assert!(nudge.contains("Do not redo the work"));
    }
}
