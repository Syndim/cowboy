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
}
