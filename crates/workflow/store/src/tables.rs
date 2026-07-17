use redb::TableDefinition;

/// Immutable content-addressed objects keyed by hash.
pub const OBJECTS: TableDefinition<&str, &[u8]> = TableDefinition::new("objects");
/// Mutable workflow run snapshots keyed by run id.
pub const RUNS: TableDefinition<&str, &[u8]> = TableDefinition::new("runs");
/// Mutable run heads keyed by run id.
pub const RUN_HEADS: TableDefinition<&str, &[u8]> = TableDefinition::new("run_heads");
/// Backend role sessions keyed by `run_id:role_id`.
pub const ROLE_SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("role_sessions");
/// Turn hash lists keyed by `run_id:step_record_id`.
pub const RUN_TURNS: TableDefinition<&str, &[u8]> = TableDefinition::new("run_turns");
/// Ordered durable follow-up prompts keyed by run id.
pub const RUN_USER_PROMPTS: TableDefinition<&str, &[u8]> = TableDefinition::new("run_user_prompts");
/// Current open or sealed agent prompt window keyed by run id.
pub const AGENT_PROMPT_WINDOWS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("agent_prompt_windows");
