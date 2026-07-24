use cowboy_agent_client::PromptContent;
use cowboy_workflow_core::{AgentAction, OutputSpec, RoleDefinition, RunUserInput, RunUserPrompt};
use serde_json::Value;

const BLOCKED_STATUS_POLICY: &str = "## Blocked Status Policy\n\n\
`blocked` is a last resort. Before choosing it, exhaust reasonable, safe, in-scope actions available through the repository, supplied context, and tools: inspect relevant context, diagnose failures, try reasonable safe fixes, and try viable in-scope alternatives. A crash, failing command or test, unfamiliar code, or an unsuccessful first approach does not by itself justify `blocked`.\n\n\
Choose `blocked` only when a precise prerequisite cannot be obtained or resolved with the available tools and context and requires a human action, decision, credential, permission, or external resource. A blocked response MUST document what was tried, the evidence that rules out self-service recovery, and the exact human help needed to continue.";

pub fn build_agent_prompt(
    role: &RoleDefinition,
    action: &AgentAction,
    user_inputs: &[RunUserInput],
    include_role: bool,
) -> String {
    let mut parts = Vec::new();
    if include_role && !role.instructions.trim().is_empty() {
        parts.push(format!("## Role\n\n{}", role.instructions.trim()));
    }
    parts.push(format!("## Task\n\n{}", action.prompt.trim()));
    if !user_inputs.is_empty() {
        let header = if include_role {
            "All entries below are cumulative user direction. Apply them in sequence."
        } else {
            "New user direction not yet sent in this session. Apply in sequence."
        };
        parts.push(format!(
            "## User Inputs\n\n{header}\n\n```json\n{}\n```",
            serde_json::to_string_pretty(user_inputs).expect("run user inputs serialize")
        ));
    }
    if let Some(output) = &action.output {
        parts.push(build_output_instruction(output));
    }
    parts.join("\n\n")
}

pub(crate) fn build_correction_prompt(
    action: &AgentAction,
    prompts: &[RunUserPrompt],
) -> Vec<PromptContent> {
    let mut blocks = Vec::with_capacity(prompts.len() * 2 + 2);
    blocks.push(PromptContent::text(
        "These entries are new cumulative user direction for the current step. Revise work already performed and replace the prior result. Return a complete replacement response, not a patch or commentary, and satisfy the original allowed statuses, fields, body expectations, and YAML-frontmatter rules.",
    ));
    for prompt in prompts {
        blocks.push(PromptContent::text(format!(
            "Follow-up user input sequence {} submitted at {}:",
            prompt.sequence,
            prompt
                .submitted_at
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        )));
        blocks.push(PromptContent::text(prompt.content.clone()));
    }
    blocks.push(PromptContent::text(match &action.output {
        Some(output) => build_output_instruction(output),
        None => "Return a complete replacement response beginning with YAML frontmatter containing a `status` field.".to_string(),
    }));
    blocks
}

/// Marker substring identifying a retry `reason` produced by a no-result reply
/// (`Error::NoWorkflowResult`). Matches both the bare variant message and the
/// wrapped runner form `recoverable action failure: agent reply did not contain
/// a workflow result`.
const NO_RESULT_REASON_MARKER: &str = "did not contain a workflow result";

/// Whether the retry `reason` indicates the previous reply carried no parseable
/// workflow result (as opposed to a malformed frontmatter block).
fn is_no_result_reason(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| reason.contains(NO_RESULT_REASON_MARKER))
}

/// Build a corrective instruction appended to a retry prompt after a parse or
/// no-result failure.
///
/// For a malformed-frontmatter reason the nudge reuses the required-frontmatter
/// description so the agent re-emits its already-completed work with a valid
/// frontmatter block; the precise `reason` (surfaced in parentheses) names the
/// specific defect.
///
/// For a no-result reason (`Error::NoWorkflowResult`) the previous reply carried
/// no parseable workflow result, and Cowboy cannot tell a stalled/interrupted
/// turn from a completed turn whose final prose omitted frontmatter. The nudge is
/// therefore best-effort and side-effect-safe: inspect existing work, continue or
/// complete only what remains without repeating completed side effects, then
/// return a complete workflow result.
pub fn build_retry_nudge(action: &AgentAction, reason: Option<&str>) -> String {
    let mut nudge = if is_no_result_reason(reason) {
        let mut nudge = String::from(
            "## Retry\n\nYour previous turn did not produce a parseable workflow result",
        );
        if let Some(reason) = reason {
            nudge.push_str(&format!(" ({reason})"));
        }
        nudge.push_str(
            ".\n\nInspect the existing work and conversation state. Continue or complete any \
unfinished work as needed, without repeating actions or side effects already completed (for \
example edited files, run commands, or commits). Then return one complete workflow result: \
begin the response with an opening `---` line, include the frontmatter fields (with a `status` \
field), and end the frontmatter with a closing `---` line on its own before the Markdown body.",
        );
        nudge
    } else {
        let mut nudge = String::from(
            "## Retry\n\nYour previous response could not be parsed as a workflow result",
        );
        if let Some(reason) = reason {
            nudge.push_str(&format!(" ({reason})"));
        }
        nudge.push_str(
            ".\n\nDo not redo the work. Re-emit your result now as a complete replacement with a \
valid YAML frontmatter block: begin the response with an opening `---` line, include the \
frontmatter fields (with a `status` field), and end the frontmatter with a closing `---` line \
on its own before the Markdown body.",
        );
        nudge
    };
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
    let fields = describe_fields(&output.fields, &output.required_fields);
    let mut instruction = format!(
        "## Deliverable Format\n\n\
Your response MUST begin with valid YAML frontmatter followed by Markdown body. Quote frontmatter strings and list items that contain `: `, backticks, brackets, braces, or other YAML punctuation.\n\n\
Allowed status values: {statuses}\n\n\
Frontmatter fields:\n{fields}\n\n\
Example:\n\n---\nstatus: success\nsummary: short summary\n---\n\nMarkdown details here."
    );

    if output.statuses.iter().any(|status| status == "blocked") {
        instruction.push_str("\n\n");
        instruction.push_str(BLOCKED_STATUS_POLICY);
    }

    instruction
}

fn describe_fields(fields: &Value, required_fields: &[String]) -> String {
    let Some(object) = fields.as_object() else {
        return "- status: routing status string".to_string();
    };
    let mut lines = vec!["- status: routing status string".to_string()];
    for (key, value) in object {
        let requirement = if required_fields.iter().any(|field| field == key) {
            "required"
        } else {
            "optional"
        };
        lines.push(format!(
            "- {key}: {} ({requirement})",
            field_description(value)
        ));
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
    use chrono::DateTime;
    use cowboy_workflow_core::RunUserInputKind;

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
                statuses: vec![
                    "success".into(),
                    "failed".into(),
                    "needs_fix".into(),
                    "unblocked".into(),
                ],
                fields: serde_json::json!({"summary": "string"}),
                required_fields: vec!["summary".into()],
            }),
        };
        let prompt = build_agent_prompt(&role, &action, &[], true);
        assert!(prompt.contains("Implement changes"));
        assert!(prompt.contains("Do work"));
        assert!(prompt.contains("valid YAML frontmatter"));
        assert!(prompt.contains("success, failed, needs_fix, unblocked"));
        assert!(prompt.contains("summary"));
        assert!(!prompt.contains("Blocked Status Policy"));
    }

    #[test]
    fn prompt_includes_policy_when_blocked_status_is_allowed() {
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
                statuses: vec!["implemented".into(), "blocked".into()],
                fields: serde_json::json!({"summary": "string"}),
                required_fields: vec![],
            }),
        };

        let prompt = build_agent_prompt(&role, &action, &[], true);

        assert!(prompt.contains("`blocked` is a last resort"));
        assert!(prompt.contains("exhaust reasonable, safe, in-scope actions"));
        assert!(prompt.contains("A crash, failing command or test, unfamiliar code"));
        assert!(prompt.contains("document what was tried"));
        assert!(prompt.contains("evidence that rules out self-service recovery"));
        assert!(prompt.contains("exact human help needed to continue"));
    }

    #[test]
    fn prompt_includes_each_user_input_exactly_once() {
        let role = RoleDefinition {
            id: "planner".into(),
            instructions: "Plan focused work".into(),
            agent: None,
            properties: Value::Null,
        };
        let action = AgentAction {
            role: "planner".into(),
            prompt: "Create the implementation plan without repeating user inputs.".into(),
            output: None,
        };
        let timestamp = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let user_inputs = vec![
            RunUserInput {
                sequence: 0,
                kind: cowboy_workflow_core::RunUserInputKind::Initial,
                content: "INITIAL_INPUT_SENTINEL".into(),
                submitted_at: timestamp,
            },
            RunUserInput {
                sequence: 1,
                kind: cowboy_workflow_core::RunUserInputKind::FollowUp,
                content: "FOLLOW_UP_INPUT_SENTINEL".into(),
                submitted_at: timestamp,
            },
        ];

        let prompt = build_agent_prompt(&role, &action, &user_inputs);

        assert_eq!(prompt.matches("## Role").count(), 1);
        assert_eq!(prompt.matches("## Task").count(), 1);
        assert_eq!(prompt.matches("## User Inputs").count(), 1);
        assert_eq!(prompt.matches("INITIAL_INPUT_SENTINEL").count(), 1);
        assert_eq!(prompt.matches("FOLLOW_UP_INPUT_SENTINEL").count(), 1);
    }

    #[test]
    fn retry_nudge_includes_reason_and_frontmatter_instruction() {
        let action = AgentAction {
            role: "dev".into(),
            prompt: "Do work".into(),
            output: Some(OutputSpec {
                statuses: vec!["success".into(), "blocked".into()],
                fields: serde_json::json!({"summary": "string"}),
                required_fields: vec![],
            }),
        };
        let nudge = build_retry_nudge(&action, Some("missing YAML frontmatter"));
        assert!(nudge.contains("Retry"));
        assert!(nudge.contains("missing YAML frontmatter"));
        assert!(nudge.contains("YAML frontmatter"));
        assert!(nudge.contains("opening `---`"));
        assert!(nudge.contains("closing `---`"));
        assert!(nudge.contains("status"));
        assert!(nudge.contains("Do not redo the work"));
        assert!(nudge.contains(BLOCKED_STATUS_POLICY));
    }

    #[test]
    fn retry_nudge_surfaces_precise_closing_delimiter_reason() {
        let action = AgentAction {
            role: "dev".into(),
            prompt: "Do work".into(),
            output: None,
        };
        let reason =
            "agent response has an opening `---` but is missing the closing `---` delimiter";
        let nudge = build_retry_nudge(&action, Some(reason));
        assert!(nudge.contains(reason));
        assert!(nudge.contains("closing `---`"));
    }

    #[test]
    fn retry_nudge_no_result_reason_is_side_effect_safe() {
        let action = AgentAction {
            role: "dev".into(),
            prompt: "Do work".into(),
            output: Some(OutputSpec {
                statuses: vec!["success".into(), "blocked".into()],
                fields: serde_json::json!({"summary": "string"}),
                required_fields: vec![],
            }),
        };
        // Full wrapped runner-style reason threaded through context.retry_reason.
        let reason = "recoverable action failure: agent reply did not contain a workflow result";
        let nudge = build_retry_nudge(&action, Some(reason));
        // (a) acknowledges no parseable result was received.
        assert!(nudge.contains("did not produce a parseable workflow result"));
        // (b) inspect / continue-or-complete-as-needed guidance.
        assert!(nudge.contains("Inspect the existing work"));
        assert!(nudge.contains("Continue or complete any unfinished work"));
        // (c) do-not-repeat-completed-side-effects instruction.
        assert!(nudge.contains("without repeating actions or side effects already completed"));
        // (d) complete workflow-result / status / YAML-frontmatter requirement.
        assert!(nudge.contains("one complete workflow result"));
        assert!(nudge.contains("`status`"));
        assert!(nudge.contains("opening `---`"));
        assert!(nudge.contains("closing `---`"));
        // (e) must NOT reuse the malformed-frontmatter wording.
        assert!(!nudge.contains("Do not redo the work"));
    }

    #[test]
    fn retry_nudge_none_reason_uses_frontmatter_wording() {
        let action = AgentAction {
            role: "dev".into(),
            prompt: "Do work".into(),
            output: None,
        };
        let nudge = build_retry_nudge(&action, None);
        assert!(nudge.contains("Do not redo the work"));
        assert!(!nudge.contains("did not produce a parseable workflow result"));
    }

    fn user_input(sequence: u64, kind: RunUserInputKind, content: &str) -> RunUserInput {
        RunUserInput {
            sequence,
            kind,
            content: content.into(),
            submitted_at: DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        }
    }

    #[test]
    fn follow_up_prompt_omits_role_but_keeps_task_and_deliverable() {
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
                statuses: vec!["success".into()],
                fields: serde_json::json!({"summary": "string"}),
                required_fields: vec!["summary".into()],
            }),
        };
        let prompt = build_agent_prompt(&role, &action, &[], false);
        assert!(!prompt.contains("## Role"));
        assert!(!prompt.contains("Implement changes"));
        assert!(prompt.contains("## Task"));
        assert!(prompt.contains("Do work"));
        assert!(prompt.contains("valid YAML frontmatter"));
    }

    #[test]
    fn empty_inputs_omit_user_inputs_and_delta_uses_follow_up_header() {
        let role = RoleDefinition {
            id: "dev".into(),
            instructions: "Implement changes".into(),
            agent: None,
            properties: Value::Null,
        };
        let action = AgentAction {
            role: "dev".into(),
            prompt: "Do work".into(),
            output: None,
        };
        let empty = build_agent_prompt(&role, &action, &[], false);
        assert!(!empty.contains("## User Inputs"));

        let delta = vec![user_input(2, RunUserInputKind::FollowUp, "new direction")];
        let prompt = build_agent_prompt(&role, &action, &delta, false);
        assert!(prompt.contains("## User Inputs"));
        assert!(prompt.contains("New user direction not yet sent in this session"));
        assert!(!prompt.contains("cumulative user direction"));
        assert!(prompt.contains("new direction"));
        assert!(prompt.contains("\"sequence\": 2"));
    }

    #[test]
    fn full_history_uses_cumulative_header() {
        let role = RoleDefinition {
            id: "dev".into(),
            instructions: "Implement changes".into(),
            agent: None,
            properties: Value::Null,
        };
        let action = AgentAction {
            role: "dev".into(),
            prompt: "Do work".into(),
            output: None,
        };
        let inputs = vec![user_input(0, RunUserInputKind::Initial, "initial request")];
        let prompt = build_agent_prompt(&role, &action, &inputs, true);
        assert!(prompt.contains("cumulative user direction"));
        assert!(!prompt.contains("New user direction not yet sent"));
    }

    #[test]
    fn no_output_spec_omits_deliverable_format_for_both_role_flags() {
        let role = RoleDefinition {
            id: "dev".into(),
            instructions: "Implement changes".into(),
            agent: None,
            properties: Value::Null,
        };
        let action = AgentAction {
            role: "dev".into(),
            prompt: "Do work".into(),
            output: None,
        };
        for include_role in [true, false] {
            let prompt = build_agent_prompt(&role, &action, &[], include_role);
            assert!(prompt.contains("## Task"));
            assert!(prompt.contains("Do work"));
            assert!(!prompt.contains("## Deliverable Format"));
        }
    }
}
