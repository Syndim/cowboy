use cowboy_agent_client::AgentSessionDescriptor;

#[test]
fn agent_session_descriptor_is_publicly_reachable_with_field_defaults() {
    let descriptor = AgentSessionDescriptor::default();
    assert!(descriptor.model.is_none());
    assert!(descriptor.context.is_none());
    assert!(descriptor.reasoning.is_none());

    let populated = AgentSessionDescriptor {
        model: Some("gpt-5.6-sol".to_string()),
        context: Some("1m".to_string()),
        reasoning: Some("high".to_string()),
    };
    assert_eq!(populated.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(populated.context.as_deref(), Some("1m"));
    assert_eq!(populated.reasoning.as_deref(), Some("high"));
}
