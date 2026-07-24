use cowboy_workflow_agent::build_agent_prompt;
use cowboy_workflow_core::{RunUserInput, RunUserInputKind, StepAction, WorkflowSourceRef};
use cowboy_workflow_lua::{load, run_step};

#[test]
fn actual_example_lua_prompt_includes_each_user_input_once() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("examples/workflows");
    let compiled = load(&WorkflowSourceRef {
        id: "feature".into(),
        root: Some(root.to_string_lossy().into_owned()),
        entry: "workflows/feature.lua".into(),
        description: None,
    })
    .unwrap();
    let timestamp = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let user_inputs = vec![
        RunUserInput {
            sequence: 0,
            kind: RunUserInputKind::Initial,
            content: "ACTUAL_INITIAL_REQUEST_SENTINEL".into(),
            submitted_at: timestamp,
        },
        RunUserInput {
            sequence: 1,
            kind: RunUserInputKind::FollowUp,
            content: "ACTUAL_FOLLOW_UP_SENTINEL".into(),
            submitted_at: timestamp,
        },
    ];
    let result = run_step(
        &compiled.source_bundle,
        "plan",
        serde_json::json!({
            "request": "ACTUAL_INITIAL_REQUEST_SENTINEL",
            "user_inputs": user_inputs,
            "run_id": "run-1",
            "workflow": { "name": "feature", "head": "plan" },
            "current_step": "plan",
            "step": { "id": "plan", "role": "planner", "properties": {} },
            "steps_executed": 0
        }),
    )
    .unwrap();
    let StepAction::Agent(action) = result.action else {
        panic!("feature plan should produce an agent action")
    };
    let role = &compiled.definition.roles[&action.role];

    assert!(!action.prompt.contains("ACTUAL_INITIAL_REQUEST_SENTINEL"));
    assert!(!action.prompt.contains("ACTUAL_FOLLOW_UP_SENTINEL"));
    assert!(!action.prompt.contains("## User Inputs"));
    assert!(
        !action
            .prompt
            .contains("All entries below are cumulative user direction")
    );

    let final_prompt = build_agent_prompt(role, &action, &user_inputs, true);
    assert_eq!(
        final_prompt
            .matches("ACTUAL_INITIAL_REQUEST_SENTINEL")
            .count(),
        1
    );
    assert_eq!(final_prompt.matches("ACTUAL_FOLLOW_UP_SENTINEL").count(), 1);
    assert_eq!(final_prompt.matches("## User Inputs").count(), 1);
}
