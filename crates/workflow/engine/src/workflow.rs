use async_trait::async_trait;
use cowboy_agent_client::{Client, Event, ModelInfo, PromptContent};
use cowboy_workflow_core::{
    Result, WorkflowCatalog, WorkflowError, WorkflowSelection, WorkflowSourceRef,
    WorkflowSummarizer, WorkflowSummary,
};
use serde::Deserialize;
use tokio::sync::Mutex;

/// Deterministic workflow selector used before an agent-backed selector exists.
#[derive(Debug, Clone, Default)]
pub struct DeterministicSelector {
    preferred_workflow: Option<String>,
}

impl DeterministicSelector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_preferred(workflow_id: impl Into<String>) -> Self {
        Self {
            preferred_workflow: Some(workflow_id.into()),
        }
    }
}

#[async_trait]
impl cowboy_workflow_core::WorkflowSelector for DeterministicSelector {
    async fn select(&self, request: &str, catalog: &WorkflowCatalog) -> Result<WorkflowSelection> {
        let selected = if let Some(preferred) = &self.preferred_workflow {
            catalog.workflows.get(preferred).ok_or_else(|| {
                WorkflowError::InvalidAction(format!("preferred workflow {preferred:?} not found"))
            })?
        } else {
            catalog.workflows.values().next().ok_or_else(|| {
                WorkflowError::InvalidAction("workflow catalog is empty".to_string())
            })?
        };

        Ok(WorkflowSelection {
            workflow_id: selected.id.clone(),
            rationale: selection_rationale(request, selected),
            confidence: 1.0,
        })
    }
}

fn selection_rationale(request: &str, selected: &WorkflowSourceRef) -> String {
    match &selected.description {
        Some(description) if !description.trim().is_empty() => {
            format!(
                "selected {:?} deterministically for request {:?}: {description}",
                selected.id, request
            )
        }
        _ => format!(
            "selected {:?} deterministically for request {:?}",
            selected.id, request
        ),
    }
}

/// Agent-backed selector that asks a coding agent to choose a workflow from a catalog.
///
/// The selector owns exactly one backend session. It validates the returned
/// workflow id against the provided catalog so a model cannot select a missing
/// workflow.
#[derive(Debug)]
pub struct AgentWorkflowSelector<C> {
    client: Mutex<C>,
    session_id: Mutex<Option<String>>,
    cwd: String,
    mcp_servers: Vec<serde_json::Value>,
    model: ModelInfo,
}

impl<C> AgentWorkflowSelector<C> {
    pub fn new(client: C, cwd: impl Into<String>, model: ModelInfo) -> Self {
        Self {
            client: Mutex::new(client),
            session_id: Mutex::new(None),
            cwd: cwd.into(),
            mcp_servers: Vec::new(),
            model,
        }
    }

    pub fn with_mcp_servers(mut self, mcp_servers: Vec<serde_json::Value>) -> Self {
        self.mcp_servers = mcp_servers;
        self
    }
}

#[async_trait]
impl<C> cowboy_workflow_core::WorkflowSelector for AgentWorkflowSelector<C>
where
    C: Client + 'static,
{
    async fn select(&self, request: &str, catalog: &WorkflowCatalog) -> Result<WorkflowSelection> {
        if catalog.workflows.is_empty() {
            return Err(WorkflowError::InvalidAction(
                "workflow catalog is empty".to_string(),
            ));
        }

        let mut client = self.client.lock().await;
        let session_id = self.ensure_session(client.as_mut_client()).await?;

        let mut prompt = selector_prompt(request, catalog);
        let mut last_text = String::new();
        for attempt in 1..=SELECTOR_ATTEMPTS {
            let mut text = String::new();
            client
                .prompt(
                    &session_id,
                    vec![PromptContent::text(prompt.clone())],
                    &mut |event| collect_text(event, &mut text),
                )
                .await
                .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
            tracing::debug!(attempt, reply = %text, "workflow selector: agent reply");
            let parsed = parse_selection_response(&text);
            last_text = text;

            match parsed {
                Ok(response) if catalog.workflows.contains_key(&response.workflow_id) => {
                    tracing::info!(
                        workflow_id = %response.workflow_id,
                        confidence = response.confidence,
                        "workflow selector: chose workflow"
                    );
                    return Ok(WorkflowSelection {
                        workflow_id: response.workflow_id,
                        rationale: response.rationale,
                        confidence: response.confidence,
                    });
                }
                Ok(response) => {
                    tracing::warn!(
                        attempt,
                        workflow_id = %response.workflow_id,
                        "workflow selector: agent picked an unknown workflow id, retrying"
                    );
                    prompt = retry_prompt(
                        &format!("{:?} is not an available workflow id", response.workflow_id),
                        catalog,
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        attempt,
                        "workflow selector: agent reply was not JSON, retrying"
                    );
                    prompt = retry_prompt("your previous reply was not a JSON object", catalog);
                }
            }
        }

        tracing::error!(
            attempts = SELECTOR_ATTEMPTS,
            reply = %agent_reply(&last_text),
            "workflow selector: no valid selection after retries"
        );
        Err(WorkflowError::InvalidAction(format!(
            "agent did not return a valid workflow selection after {SELECTOR_ATTEMPTS} attempts; last reply: {}",
            agent_reply(&last_text)
        )))
    }
}

trait AsMutClient {
    fn as_mut_client(&mut self) -> &mut dyn Client;
}

impl<C: Client> AsMutClient for C {
    fn as_mut_client(&mut self) -> &mut dyn Client {
        self
    }
}

impl<C> AgentWorkflowSelector<C>
where
    C: Client,
{
    async fn ensure_session(&self, client: &mut dyn Client) -> Result<String> {
        if let Some(session_id) = self.session_id.lock().await.clone() {
            return Ok(session_id);
        }
        if let Some(session_id) = client.session_id() {
            let session_id = session_id.to_string();
            *self.session_id.lock().await = Some(session_id.clone());
            return Ok(session_id);
        }
        let session_id = client
            .new_session(&self.cwd, &self.mcp_servers, &self.model)
            .await
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        *self.session_id.lock().await = Some(session_id.clone());
        Ok(session_id)
    }
}

fn selector_prompt(request: &str, catalog: &WorkflowCatalog) -> String {
    let workflows = catalog
        .workflows
        .values()
        .map(|workflow| {
            format!(
                "- {}: {}",
                workflow.id,
                workflow
                    .description
                    .as_deref()
                    .unwrap_or("(no description)")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Pick the workflow that best fits the user request below. This is a one-shot \
classification task: do NOT run tools, do NOT ask questions, and do NOT add any text \
outside the JSON.\n\n\
User request:\n{request}\n\n\
Available workflows (id: description):\n{workflows}\n\n\
Respond with ONLY a single JSON object, nothing else:\n\
{{\"workflow_id\": \"<one id from the list>\", \"rationale\": \"<one short sentence>\", \"confidence\": <number between 0 and 1>}}\n\
If unsure, choose the closest match with a low confidence."
    )
}

/// Number of times the agent selector asks before giving up.
const SELECTOR_ATTEMPTS: usize = 2;

/// Strict re-prompt used when the agent's previous reply was not a usable
/// selection; names the valid workflow ids so the model can correct itself.
fn retry_prompt(reason: &str, catalog: &WorkflowCatalog) -> String {
    let ids = catalog
        .workflows
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{reason}. Respond with ONLY a single JSON object and nothing else, using a \
workflow_id from: {ids}.\n\
{{\"workflow_id\": \"<id>\", \"rationale\": \"<one sentence>\", \"confidence\": <number between 0 and 1>}}"
    )
}

/// The agent's reply, trimmed, for inclusion in parse-failure error messages.
/// Returned in full so a failed run shows exactly what the agent said.
fn agent_reply(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "<empty>".to_string()
    } else {
        trimmed.to_string()
    }
}

fn collect_text(event: Event, text: &mut String) {
    if let Event::MessageChunk { content } = event
        && let Some(chunk) = content.get("text").and_then(|value| value.as_str())
    {
        text.push_str(chunk);
    }
}

#[derive(Debug, Deserialize)]
struct SelectionResponse {
    workflow_id: String,
    rationale: String,
    confidence: f64,
}

fn parse_selection_response(text: &str) -> Result<SelectionResponse> {
    let start = text.find('{').ok_or_else(|| {
        WorkflowError::InvalidAction("selector response missing JSON".to_string())
    })?;
    let end = text.rfind('}').ok_or_else(|| {
        WorkflowError::InvalidAction("selector response missing JSON".to_string())
    })?;
    serde_json::from_str(&text[start..=end])
        .map_err(|err| WorkflowError::InvalidAction(format!("invalid selector JSON: {err}")))
}

pub const REQUEST_TOPIC_MAX_CHARS: usize = 80;

/// Agent-backed generator for a compact title-bar topic from the initial request.
#[derive(Debug)]
pub struct AgentRequestTopicGenerator<C> {
    client: Mutex<C>,
    session_id: Mutex<Option<String>>,
    cwd: String,
    mcp_servers: Vec<serde_json::Value>,
    model: ModelInfo,
}

impl<C> AgentRequestTopicGenerator<C> {
    pub fn new(client: C, cwd: impl Into<String>, model: ModelInfo) -> Self {
        Self {
            client: Mutex::new(client),
            session_id: Mutex::new(None),
            cwd: cwd.into(),
            mcp_servers: Vec::new(),
            model,
        }
    }

    pub fn with_mcp_servers(mut self, mcp_servers: Vec<serde_json::Value>) -> Self {
        self.mcp_servers = mcp_servers;
        self
    }
}

impl<C> AgentRequestTopicGenerator<C>
where
    C: Client,
{
    pub async fn generate(&self, request: &str) -> Result<String> {
        let mut client = self.client.lock().await;
        let session_id = self.ensure_session(client.as_mut_client()).await?;
        let mut text = String::new();
        client
            .prompt(
                &session_id,
                vec![PromptContent::text(request_topic_prompt(request))],
                &mut |event| collect_text(event, &mut text),
            )
            .await
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        parse_request_topic_response(&text)
    }

    async fn ensure_session(&self, client: &mut dyn Client) -> Result<String> {
        if let Some(session_id) = self.session_id.lock().await.clone() {
            return Ok(session_id);
        }
        if let Some(session_id) = client.session_id() {
            let session_id = session_id.to_string();
            *self.session_id.lock().await = Some(session_id.clone());
            return Ok(session_id);
        }
        let session_id = client
            .new_session(&self.cwd, &self.mcp_servers, &self.model)
            .await
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        *self.session_id.lock().await = Some(session_id.clone());
        Ok(session_id)
    }
}

fn request_topic_prompt(request: &str) -> String {
    format!(
        "Create a compact title-bar topic for this user request. This is a one-shot \
summarization task: do NOT run tools, do NOT ask questions, and do NOT include \
Markdown or prose outside the JSON. The topic must be a non-empty single-line \
phrase, at most {REQUEST_TOPIC_MAX_CHARS} characters, ideally 2-6 words.\n\n\
User request:\n{request}\n\n\
Respond with ONLY a single JSON object, nothing else:\n\
{{\"topic\": \"<short topic>\"}}"
    )
}

#[derive(Debug, Deserialize)]
struct RequestTopicResponse {
    topic: String,
}

fn parse_request_topic_response(text: &str) -> Result<String> {
    let response: RequestTopicResponse = parse_json_response(text, "request topic")?;
    validate_request_topic(response.topic)
}

fn validate_request_topic(topic: String) -> Result<String> {
    let topic = topic.trim();
    if topic.is_empty() {
        return Err(WorkflowError::InvalidAction(
            "request topic is empty".to_string(),
        ));
    }

    if topic.contains('\n') || topic.contains('\r') {
        return Err(WorkflowError::InvalidAction(
            "request topic must be a single line".to_string(),
        ));
    }

    if topic.chars().count() > REQUEST_TOPIC_MAX_CHARS {
        return Err(WorkflowError::InvalidAction(format!(
            "request topic must be at most {REQUEST_TOPIC_MAX_CHARS} characters"
        )));
    }

    Ok(topic.to_string())
}

/// Agent-backed post-run summarizer that produces a `WorkflowSummary` JSON object.
#[derive(Debug)]
pub struct AgentWorkflowSummarizer<C> {
    client: Mutex<C>,
    session_id: Mutex<Option<String>>,
    cwd: String,
    mcp_servers: Vec<serde_json::Value>,
    model: ModelInfo,
}

impl<C> AgentWorkflowSummarizer<C> {
    pub fn new(client: C, cwd: impl Into<String>, model: ModelInfo) -> Self {
        Self {
            client: Mutex::new(client),
            session_id: Mutex::new(None),
            cwd: cwd.into(),
            mcp_servers: Vec::new(),
            model,
        }
    }

    pub fn with_mcp_servers(mut self, mcp_servers: Vec<serde_json::Value>) -> Self {
        self.mcp_servers = mcp_servers;
        self
    }
}

#[async_trait]
impl<C> WorkflowSummarizer for AgentWorkflowSummarizer<C>
where
    C: Client + 'static,
{
    async fn summarize(&self, run: &cowboy_workflow_core::WorkflowRun) -> Result<WorkflowSummary> {
        let mut client = self.client.lock().await;
        let session_id = self.ensure_session(client.as_mut_client()).await?;
        let mut text = String::new();
        client
            .prompt(
                &session_id,
                vec![PromptContent::text(summary_prompt(run)?)],
                &mut |event| collect_text(event, &mut text),
            )
            .await
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        let summary: WorkflowSummary = parse_json_response(&text, "summary")?;
        if summary.selected_workflow_id != run.workflow_name {
            return Err(WorkflowError::InvalidAction(format!(
                "summary selected workflow {:?} does not match run workflow {:?}",
                summary.selected_workflow_id, run.workflow_name
            )));
        }
        Ok(summary)
    }
}

impl<C> AgentWorkflowSummarizer<C>
where
    C: Client,
{
    async fn ensure_session(&self, client: &mut dyn Client) -> Result<String> {
        if let Some(session_id) = self.session_id.lock().await.clone() {
            return Ok(session_id);
        }
        if let Some(session_id) = client.session_id() {
            let session_id = session_id.to_string();
            *self.session_id.lock().await = Some(session_id.clone());
            return Ok(session_id);
        }
        let session_id = client
            .new_session(&self.cwd, &self.mcp_servers, &self.model)
            .await
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        *self.session_id.lock().await = Some(session_id.clone());
        Ok(session_id)
    }
}

fn summary_prompt(run: &cowboy_workflow_core::WorkflowRun) -> Result<String> {
    let run = serde_json::to_string_pretty(run)
        .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
    Ok(format!(
        "Summarize this workflow run for future workflow improvement.\n\nRun JSON:\n{run}\n\nReturn only JSON matching WorkflowSummary: goal, selected_workflow_id, steps, outcome, improvement. Use improvement {{\"kind\":\"none\",\"rationale\":\"...\"}} when no change is needed."
    ))
}

fn parse_json_response<T: serde::de::DeserializeOwned>(text: &str, label: &str) -> Result<T> {
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        return Err(WorkflowError::InvalidAction(format!(
            "{label} response missing JSON; agent reply: {}",
            agent_reply(text)
        )));
    };
    serde_json::from_str(&text[start..=end]).map_err(|err| {
        WorkflowError::InvalidAction(format!(
            "invalid {label} JSON: {err}; agent reply: {}",
            agent_reply(text)
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};

    use chrono::Utc;
    use cowboy_agent_client::{AgentInfo, StopReason};
    use cowboy_workflow_core::{
        RunStatus, WorkflowImprovement, WorkflowRun, WorkflowSelector, WorkflowSourceRef,
        WorkflowSummarizer,
    };
    use serde_json::Value;

    use super::*;

    fn catalog() -> WorkflowCatalog {
        WorkflowCatalog {
            workflows: BTreeMap::from([
                (
                    "default".to_string(),
                    WorkflowSourceRef {
                        id: "default".to_string(),
                        entry: "main.lua".to_string(),
                        root: None,
                        description: Some("built-in default workflow".to_string()),
                    },
                ),
                (
                    "special".to_string(),
                    WorkflowSourceRef {
                        id: "special".to_string(),
                        entry: "special.lua".to_string(),
                        root: None,
                        description: None,
                    },
                ),
            ]),
        }
    }

    #[derive(Debug)]
    struct FakeClient {
        session_id: Option<String>,
        responses: VecDeque<String>,
        prompts: Vec<String>,
    }

    impl FakeClient {
        fn new(response: impl Into<String>) -> Self {
            Self {
                session_id: None,
                responses: VecDeque::from([response.into()]),
                prompts: Vec::new(),
            }
        }

        fn with_responses<S: Into<String>>(responses: impl IntoIterator<Item = S>) -> Self {
            Self {
                session_id: None,
                responses: responses.into_iter().map(Into::into).collect(),
                prompts: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl Client for FakeClient {
        fn is_connected(&self) -> bool {
            true
        }

        fn agent_info(&self) -> Option<&AgentInfo> {
            None
        }

        fn session_id(&self) -> Option<&str> {
            self.session_id.as_deref()
        }

        async fn new_session(
            &mut self,
            _cwd: &str,
            _mcp_servers: &[Value],
            _model: &ModelInfo,
        ) -> anyhow::Result<String> {
            self.session_id = Some("selector-session".to_string());
            Ok("selector-session".to_string())
        }

        fn supports_load_session(&self) -> bool {
            false
        }

        async fn load_session(
            &mut self,
            _session_id: &str,
            _cwd: &str,
            _mcp_servers: &[Value],
        ) -> anyhow::Result<Vec<Event>> {
            Ok(Vec::new())
        }

        async fn prompt(
            &mut self,
            _session_id: &str,
            prompt_content: Vec<PromptContent>,
            event_handler: &mut (dyn FnMut(Event) + Send),
        ) -> anyhow::Result<StopReason> {
            self.prompts
                .extend(prompt_content.into_iter().map(|content| content.text));
            let response = self.responses.pop_front().unwrap_or_default();
            event_handler(Event::MessageChunk {
                content: serde_json::json!({ "text": response }),
            });
            Ok(StopReason::EndTurn)
        }

        async fn close(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn deterministic_selector_uses_preferred_workflow() {
        let selection = DeterministicSelector::with_preferred("special")
            .select("fix bug", &catalog())
            .await
            .unwrap();

        assert_eq!(selection.workflow_id, "special");
        assert_eq!(selection.confidence, 1.0);
    }

    #[tokio::test]
    async fn deterministic_selector_falls_back_to_first_catalog_entry() {
        let selection = DeterministicSelector::new()
            .select("fix bug", &catalog())
            .await
            .unwrap();

        assert_eq!(selection.workflow_id, "default");
    }

    #[tokio::test]
    async fn agent_selector_parses_and_validates_model_choice() {
        let selector = AgentWorkflowSelector::new(
            FakeClient::new(
                r#"{"workflow_id":"special","rationale":"best fit","confidence":0.82}"#,
            ),
            ".",
            ModelInfo::default(),
        );

        let selection = selector.select("fix bug", &catalog()).await.unwrap();

        assert_eq!(selection.workflow_id, "special");
        assert_eq!(selection.rationale, "best fit");
        assert_eq!(selection.confidence, 0.82);
    }

    #[tokio::test]
    async fn agent_selector_rejects_unknown_workflow() {
        let selector = AgentWorkflowSelector::new(
            FakeClient::new(r#"{"workflow_id":"missing","rationale":"bad","confidence":0.1}"#),
            ".",
            ModelInfo::default(),
        );

        let err = selector.select("fix bug", &catalog()).await.unwrap_err();

        assert!(matches!(err, WorkflowError::InvalidAction(_)));
    }

    #[tokio::test]
    async fn agent_selector_error_includes_raw_reply() {
        let reply = "I'd use the default workflow, but tell me more about what you want.";
        let selector = AgentWorkflowSelector::new(
            FakeClient::with_responses([reply, reply]),
            ".",
            ModelInfo::default(),
        );

        let err = selector.select("fix bug", &catalog()).await.unwrap_err();
        let WorkflowError::InvalidAction(message) = err else {
            panic!("expected InvalidAction, got {err:?}")
        };
        assert!(
            message.contains(reply),
            "selector error should include the agent reply, got: {message}"
        );
    }

    #[tokio::test]
    async fn request_topic_generator_prompts_for_json_and_trims_topic() {
        let generator = AgentRequestTopicGenerator::new(
            FakeClient::new(r#"{"topic":"  Header Topic  "}"#),
            "/repo",
            ModelInfo::default(),
        );

        let topic = generator.generate("Add compact UI chrome").await.unwrap();

        assert_eq!(topic, "Header Topic");
        let client = generator.client.lock().await;
        assert_eq!(client.prompts.len(), 1);
        let prompt = &client.prompts[0];
        assert!(
            prompt.contains("Create a compact title-bar topic"),
            "{prompt}"
        );
        assert!(prompt.contains("do NOT run tools"), "{prompt}");
        assert!(
            prompt.contains("Respond with ONLY a single JSON object"),
            "{prompt}"
        );
        assert!(prompt.contains(r#"{"topic": "<short topic>"}"#), "{prompt}");
        assert!(prompt.contains("Add compact UI chrome"), "{prompt}");
    }

    #[test]
    fn request_topic_response_rejects_invalid_title_bar_topics() {
        for (raw, expected) in [
            (r#"{"topic":"   "}"#, "request topic is empty"),
            (
                r#"{"topic":"first\nsecond"}"#,
                "request topic must be a single line",
            ),
            (
                r#"{"topic":"abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabc"}"#,
                "request topic must be at most 80 characters",
            ),
        ] {
            let err = parse_request_topic_response(raw).unwrap_err();

            assert!(
                err.to_string().contains(expected),
                "expected {expected:?} in {err}"
            );
        }
    }

    #[tokio::test]
    async fn request_topic_generator_prompts_for_json_without_tools_or_questions() {
        let generator = AgentRequestTopicGenerator::new(
            FakeClient::new(r#"{"topic":"Add health route"}"#),
            ".",
            ModelInfo::default(),
        );

        let topic = generator.generate("add a /healthz route").await.unwrap();
        let prompts = generator.client.lock().await.prompts.clone();

        assert_eq!(topic, "Add health route");
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("do NOT run tools"));
        assert!(prompts[0].contains("do NOT ask questions"));
        assert!(prompts[0].contains(r#"{"topic": "<short topic>"}"#));
        assert!(prompts[0].contains("add a /healthz route"));
    }

    #[tokio::test]
    async fn request_topic_generator_extracts_json_from_surrounding_prose() {
        let generator = AgentRequestTopicGenerator::new(
            FakeClient::new(r#"Here is JSON: {"topic":"Review branch"}."#),
            ".",
            ModelInfo::default(),
        );

        let topic = generator.generate("review this branch").await.unwrap();

        assert_eq!(topic, "Review branch");
    }

    #[tokio::test]
    async fn request_topic_generator_rejects_invalid_json() {
        let generator =
            AgentRequestTopicGenerator::new(FakeClient::new("not json"), ".", ModelInfo::default());

        let err = generator.generate("fix it").await.unwrap_err();

        assert!(
            err.to_string()
                .contains("request topic response missing JSON")
        );
    }

    #[test]
    fn request_topic_validation_rejects_empty_and_multiline_topics() {
        let empty = parse_request_topic_response(r#"{"topic":"   "}"#).unwrap_err();
        assert!(empty.to_string().contains("empty"));

        let multiline = parse_request_topic_response(r#"{"topic":"first\nsecond"}"#).unwrap_err();
        assert!(multiline.to_string().contains("single line"));
    }

    #[test]
    fn request_topic_validation_rejects_overlong_topics() {
        let topic = "x".repeat(REQUEST_TOPIC_MAX_CHARS + 1);
        let err = validate_request_topic(topic).unwrap_err();

        assert!(err.to_string().contains("at most"));
    }

    #[tokio::test]
    async fn agent_summarizer_parses_workflow_summary() {
        let now = Utc::now();
        let run = WorkflowRun {
            id: "run-1".to_string(),
            workflow_name: "default".to_string(),
            workflow_api_version: 1,
            workflow_hash: "hash".to_string(),
            workflow_sources: BTreeMap::new(),
            original_request: "do it".to_string(),
            request_topic: None,
            status: RunStatus::Completed,
            current_step: "finish".to_string(),
            head: None,
            resume: serde_json::Value::Null,
            steps_executed: 0,
            step_visits: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        };
        let summarizer = AgentWorkflowSummarizer::new(
            FakeClient::new(
                r#"{
                  "goal":"do it",
                  "selected_workflow_id":"default",
                  "steps":[],
                  "outcome":"completed",
                  "improvement":{"kind":"none","rationale":"already good"}
                }"#,
            ),
            ".",
            ModelInfo::default(),
        );

        let summary = summarizer.summarize(&run).await.unwrap();

        assert_eq!(summary.goal, "do it");
        assert_eq!(summary.outcome, "completed");
        assert!(matches!(
            summary.improvement,
            WorkflowImprovement::None { .. }
        ));
    }
}
