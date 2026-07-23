## Plan

Add a per-agent ACP watchdog that detects inactivity during an active
`session/prompt` turn and recovers in two ordered stages without creating a new
workflow session:

1. If no valid inbound ACP message is received for the configured response
   timeout, send exactly one `session/cancel` notification for the active
   session.
2. Wait a separate configured cancellation timeout for the matching prompt
   response. Session updates and permission requests may still be processed
   during this grace period, but they must not extend the cancellation deadline.
3. If the response confirms cancellation, send the existing `"Continue"`
   follow-up prompt in the same session. If the original turn completes normally
   before cancellation takes effect, accept that completion and do not send a
   duplicate continuation.
4. `session/cancel` is a JSON-RPC notification, so it has no response and cannot
   itself return an RPC error. Escalate when writing that notification fails,
   when the outstanding `session/prompt` response contains an RPC error, when
   the transport reaches EOF or emits malformed JSON, or when the matching
   prompt response does not arrive before the cancellation timeout.
5. On escalation, force-terminate the current transport. For the production
   stdio transport this must kill the owned child process using its recorded
   PID, wait for process exit, and discard the dead transport.
6. Reconnect through the existing ACP transport creation path with the current
   session id, which already launches the replacement agent with
   `--resume=<session-id>` and performs ACP initialization, then send
   `"Continue"` to resume the interrupted work.

The response timeout is an inactivity timeout, not a total turn duration. Arm it
after `session/prompt` is written and reset it after every successfully parsed
inbound ACP message, including `session/update`, permission requests, and
JSON-RPC responses. Empty lines and valid-but-unrecognized JSON are skipped and
do not count as activity. Malformed JSON is not treated as silence: it is a
protocol failure. During the original turn it escalates directly to hard
recovery because the stream is no longer trustworthy; during cancellation
grace it means cancellation could not be confirmed and also escalates. A
malformed initialize or recovery-continuation response from a replacement
transport terminates and disposes that replacement before returning an error.
Apply the same watchdog to the initial prompt, automatic continuation prompts,
mid-run correction prompts, and agent-backed request-topic generation because
all of those paths use `cowboy_agent_acp::Client::prompt`.

Add a nested watchdog policy to every `[[agents]]` entry:

```toml
[agents.watchdog]
response_timeout_seconds = 100
cancel_timeout_seconds = 10
recovery_operation_timeout_seconds = 30
```

Use these conservative values as defaults so existing configuration files
remain valid and watchdog protection is enabled without requiring a migration.
Reject zero values. The recovery-operation timeout independently bounds each
force-termination wait, replacement transport creation, ACP initialization, and
recovery `"Continue"` request write. Waiting for the continuation response is
then governed by the normal response inactivity timeout. Configuration remains
process-local like the other agent settings, so a long-lived TUI must be
restarted to pick up changes.

Do not start a replacement process until force termination of the old transport
has completed within its bound. If termination fails or times out, take and drop
the old transport (retaining `kill_on_drop(true)` as the last attempt), return a
specific recovery error, and let the existing workflow retry policy decide
whether to retry the step. If replacement creation or initialization fails or
times out, force-terminate and remove any partially connected replacement
within the same bounded cleanup policy. If the recovery `"Continue"` write or
its immediate protocol/RPC handling fails, terminate and remove the replacement
before returning the error. A later inactivity timeout after a successfully
dispatched continuation starts a new watchdog recovery cycle.

Keep watchdog recovery below `cowboy-workflow-agent`: it must not advance
workflow retry counters, replace the durable `RoleSession`, reopen the prompt
window, or resend the full workflow task prompt. Only return an error to the
existing workflow retry policy when force termination, reconnect/resume,
initialization, or the recovery continuation fails. Existing accepted
on-the-fly user-prompt cancellation must retain priority over watchdog recovery
and must not cause an automatic `"Continue"` prompt. Preserve the current
message-first race behavior, but order the remaining biased wait branches as
external `PromptTurnCancellation` before watchdog expiry. Therefore, if external
cancellation and the inactivity deadline become ready in the same poll,
external cancellation sends the ordinary single `session/cancel` path and wins
without watchdog continuation or transport restart.

## Changes

- In `crates/tui/app/src/config.rs`:
  - add `AgentWatchdogConfig` with default
    `response_timeout_seconds = 100`, `cancel_timeout_seconds = 10`, and
    `recovery_operation_timeout_seconds = 30`;
  - add `watchdog` to `AgentConfig`, parse `[agents.watchdog]`, validate all three
    values are greater than zero, and preserve them during runtime conversion;
  - update config fixtures and construction sites for the new defaulted field.
- In `crates/workflow/engine/src/runtime.rs`,
  `crates/workflow/engine/src/runtime_dependencies.rs`, and
  `crates/workflow/engine/src/bin/engine-cli.rs`:
  - carry the watchdog policy in `AgentRuntimeConfig`;
  - add an engine-private injectable ACP connector that accepts the resolved
    transport configuration and `AgentWatchdogOptions`, while the production
    implementation delegates to the real ACP connection constructor;
  - route explicit named-role, default-agent request-topic, default-agent
    workflow-selection, and default-agent workflow-improvement client creation
    through that connector so tests observe each production call site rather
    than only a watchdog-mapping helper;
  - update runtime test literals without moving watchdog logic into the engine.
- In `crates/agent/acp/src/client.rs` and `crates/agent/acp/src/lib.rs`:
  - add serializable/defaulted ACP client watchdog options and a connection
    constructor that accepts them while retaining the current default
    constructor;
  - extract the existing `"Continue"` text into one constant reused by empty-turn
    continuation and watchdog recovery;
  - make the single-turn receive loop arm/reset the inactivity deadline around
    each parsed inbound message;
  - distinguish external prompt cancellation from watchdog-triggered
    cancellation so only the watchdog path retries with `"Continue"`;
  - use an explicit biased select order of inbound message, external
    cancellation, then watchdog timeout so simultaneous external cancellation
    and inactivity expiry cannot enter watchdog recovery;
  - enforce a fixed cancellation grace deadline, handle the normal-completion
    race, and distinguish notification-send failure from an error carried by the
    outstanding prompt response;
  - skip empty and valid-but-unrecognized input without resetting inactivity,
    while treating malformed JSON as a protocol failure with the phase-specific
    recovery behavior defined above;
  - after hard termination, reconnect with the same session id, initialize the
    replacement agent, clear stale pushback state, and continue the prompt;
  - apply `recovery_operation_timeout_seconds` independently to termination,
    replacement creation, initialization, and continuation dispatch;
  - centralize failed-replacement cleanup so initialization, continuation-send,
    and immediate continuation protocol/RPC failures always terminate and remove
    the replacement before an error escapes;
  - emit structured tracing with stable `event` fields
    `agent_watchdog_timeout`, `agent_watchdog_cancel_sent`,
    `agent_watchdog_soft_recovered`, `agent_watchdog_force_terminated`,
    `agent_watchdog_transport_resumed`, and
    `agent_watchdog_recovery_failed`, without logging prompt contents or
    credentials.
- In `crates/agent/acp/src/transport/mod.rs`,
  `crates/agent/acp/src/transport/stdio.rs`, and
  `crates/agent/acp/src/transport/zellij.rs`:
  - add an explicit force-termination operation separate from ordinary close;
  - implement stdio termination through the owned `tokio::process::Child`,
    logging the recorded PID and waiting for process exit;
  - implement the transport-equivalent pane termination for Zellij and a
    deterministic implementation for test transports;
  - leave `kill_on_drop(true)` as a final safety net.
- Add deterministic ACP client test seams that provide an initial controlled
  transport plus a replacement-transport factory with ready, error, and pending
  outcomes. Record replacement-factory calls and transport drop/termination
  counts so tests prove the replacement is not requested before forced
  reconnect and that every failed or timed-out recovery phase disposes it. The
  seam must exercise the production reconnection state machine rather than
  bypassing it.
- In `crates/agent/acp/Cargo.toml`:
  - add Tokio's `test-util` feature to the dev-dependency used by ACP tests so
    `#[tokio::test(start_paused = true)]`, `tokio::time::pause`, and
    `tokio::time::advance` are available; production Tokio features remain
    unchanged;
  - add UUID v4 support for scenario invocation tokens and process-start nonces
    used only by the watchdog fixture identity protocol.
- Add `crates/agent/acp/src/bin/watchdog-fixture.rs`, a deterministic ACP test
  app with:
  - `serve --mode acknowledge-cancel|ignore-cancel --events <jsonl>
    --invocation-token <token> --identity-dir <dir>` for Cowboy to launch as the
    configured agent;
  - support for the existing `--resume=<session-id>` restart argument;
  - a cryptographically random scenario invocation token plus a distinct UUID
    process-start nonce generated by each initial/replacement process; canonical
    executable path, PID, token, nonce, and loopback control endpoint are written
    to one nonce-named identity file and the process-start JSONL event;
  - JSONL records for process start identity, initialize, session id, prompt
    text, cancel receipt, resume id, and final response;
  - `verify --cowboy <path> --workspace <path> ...` to create isolated configs
    and a one-step `watchdog_smoke.lua`, run both recovery scenarios, enforce
    time bounds, compare PID/session/identity values, verify Cowboy log markers,
    and clean live fixtures through authenticated control endpoints; factor the
    scenario runner behind an injectable test seam so unit tests can force
    success or failure after evidence has been written and observe workspace
    retention/removal;
  - `cleanup --workspace <path>` to challenge each recorded control endpoint
    with its invocation token and process-start nonce, require the live process
    to return the same PID and canonical executable path, and only then request
    self-termination. It must refuse to signal or kill anything when the
    endpoint is unavailable or any identity field mismatches, preventing a
    reused PID from being targeted.
- Update `README.md`, `demo-config.toml`, `docs/architecture.md`, and
  `docs/module-map.md` with the watchdog fields, defaults, inactivity semantics,
  escalation order, restart requirement for config edits, and distinction from
  workflow retries. Give each Markdown file one uniquely marked authoritative
  watchdog contract block and avoid duplicate watchdog field assignments
  elsewhere. Add
  `config::tests::documented_agent_watchdog_contract_is_unique_and_exact`,
  which parses the demo config, extracts and compares each authoritative block,
  and rejects missing, duplicate, or contradictory field assignments and
  recovery semantics.

## Tests to be added/updated

- `crates/tui/app/src/config.rs` unit tests:
  - missing watchdog configuration receives all three defaults;
  - explicit per-agent values parse and survive `runtime_config`;
  - zero response, cancellation, or recovery-operation timeouts are rejected
    with field-specific errors;
  - the shipped demo config matches the documented watchdog policy.
- `crates/workflow/engine/src/runtime_dependencies.rs` and runtime tests:
  - `role_agent_client_construction_uses_explicit_named_watchdog` invokes the
    production named-role path and records that the selected named agent's
    watchdog values reach the ACP connector;
  - `request_topic_client_construction_uses_default_watchdog` invokes the
    production request-topic path and records the default agent's values;
  - `workflow_selector_client_construction_uses_default_watchdog` invokes
    selection through `WorkflowRuntime` and records the default agent's values;
  - `workflow_improvement_client_construction_uses_default_watchdog` invokes
    improvement summarization through `WorkflowRuntime` and records the default
    agent's values;
  - the named-role test also uses distinct default/named commands and watchdog
    values so it proves existing named-agent resolution, not only option
    conversion.
- `crates/agent/acp/src/client.rs` async unit tests with paused Tokio time and
  controlled transports:
  - `watchdog_options_retain_defaults_and_explicit_values` and
    `replacement_transport_is_consumed_only_after_forced_reconnect` prove option
    retention and replacement-factory consumption timing;
  - parsed activity repeatedly resets the response deadline and no cancel is
    sent while activity continues;
  - inactivity sends one `session/cancel`; a timely `cancelled` response causes
    one `"Continue"` prompt on the same transport and session;
  - a normal prompt response racing with cancellation wins without a duplicate
    continuation;
  - updates during cancellation grace do not extend the fixed cancellation
    timeout;
  - cancellation timeout force-terminates the original transport, reconnects
    with the same session id, initializes the replacement transport, and sends
    `"Continue"`;
  - a `session/cancel` notification write failure uses hard recovery;
  - an RPC error on the outstanding `session/prompt` response during cancel
    grace uses hard recovery;
  - valid-but-unrecognized JSON does not reset inactivity, while malformed JSON
    during the original turn or cancellation grace uses hard recovery;
  - external `PromptTurnCancellation` still returns `StopReason::Cancelled`
    without watchdog continuation or transport restart;
  - when external `PromptTurnCancellation` and the watchdog deadline become
    ready on the same paused-time advance, external cancellation wins, exactly
    one ordinary `session/cancel` is sent, and no watchdog `"Continue"` or
    transport restart occurs;
  - a second inactivity period after successful recovery is monitored again;
  - force termination, replacement creation, initialization, and recovery
    continuation dispatch each time out at the configured operation bound;
  - initialization failure/timeout and recovery-continuation
    send/protocol/RPC failure terminate and dispose the replacement transport
    before returning an error for the existing workflow retry policy.
- `crates/agent/acp/src/transport/stdio.rs` unit test:
  - spawn a long-running child, record its PID, force-terminate the transport,
    and assert the child has exited and subsequent receive cannot report a live
    connection.
- Add named Zellij, mock, and controlled-transport tests for the new
  force-termination contract so each implementation can be invoked directly.
- Add fixture self-tests for ACP request handling, `--resume` parsing, JSONL
  records, authenticated process identity, cleanup refusal on PID/token/start
  nonce/executable mismatch, successful self-shutdown after a full identity
  match, and the generated smoke workflow/config. Add
  `watchdog_fixture_verify_failure_preserves_evidence_workspace` and
  `watchdog_fixture_verify_success_removes_workspace` so verifier lifecycle
  behavior is directly observed before relying on the end-to-end verifier.
- Keep the existing ACP cancellation, automatic continuation, session reuse,
  prompt-window correction, and workflow retry tests passing to prove watchdog
  recovery does not change their semantics.

## How to verify

1. Run `cargo test -p cowboy config::tests` and confirm all agent watchdog
   default, parsing, three-field validation, demo-config, and runtime-conversion
   tests pass.
2. Run
   `cargo test -p cowboy documented_agent_watchdog_contract_is_unique_and_exact`
   and confirm it parses the shipped demo config, extracts exactly one
   authoritative watchdog block from each Markdown file, binds the three fields
   to `100`/`10`/`30`, and rejects duplicate or contradictory assignments and
   recovery-order text.
3. Run these production-construction tests:
   - `cargo test -p cowboy-workflow-engine role_agent_client_construction_uses_explicit_named_watchdog`
   - `cargo test -p cowboy-workflow-engine request_topic_client_construction_uses_default_watchdog`
   - `cargo test -p cowboy-workflow-engine workflow_selector_client_construction_uses_default_watchdog`
   - `cargo test -p cowboy-workflow-engine workflow_improvement_client_construction_uses_default_watchdog`

   Confirm each test invokes its real runtime path through the recording ACP
   connector, and that the role test selects the named agent rather than the
   deliberately different default agent.
4. Run
   `cargo test -p cowboy-agent-acp watchdog_options_retain_defaults_and_explicit_values`
   and
   `cargo test -p cowboy-agent-acp replacement_transport_is_consumed_only_after_forced_reconnect`.
   Then run `cargo test -p cowboy-agent-acp client::tests` to execute the complete
   paused-time soft/hard recovery branch matrix, followed by
   `cargo test -p cowboy-agent-acp external_cancellation_wins_simultaneous_watchdog_deadline`
   as a dedicated race proof. Confirm the suite contains and passes the named
   soft/hard/error/timeout/repetition tests specified by TODO-04, TODO-06, and
   TODO-07; no expected result may be inferred from a filter that selects only a
   happy path.
5. Run each transport-contract command:
   - `cargo test -p cowboy-agent-acp force_terminate_stops_stdio_child_by_pid`
   - `cargo test -p cowboy-agent-acp force_terminate_closes_zellij_pane_once`
   - `cargo test -p cowboy-agent-acp force_terminate_disposes_mock_transport`
   - `cargo test -p cowboy-agent-acp force_terminate_disposes_controlled_transport`

   Confirm the stdio PID exits, Zellij issues one pane close, and both test
   transports record disposal.
6. Run `cargo test -p cowboy-agent-acp watchdog_fixture` and confirm fixture ACP
   handling, resume parsing, JSONL event recording, authenticated identity,
   cleanup refusal on every mismatch, matched self-shutdown, generated
   workflow/config, failed-verification evidence retention, and
   successful-verification workspace removal tests pass. Run the two lifecycle
   tests by exact name as well:
   - `cargo test -p cowboy-agent-acp watchdog_fixture_verify_failure_preserves_evidence_workspace`
   - `cargo test -p cowboy-agent-acp watchdog_fixture_verify_success_removes_workspace`
7. Run `cargo test -p cowboy-workflow-agent` and confirm session reuse,
   correction prompts, accepted mid-run prompts, and workflow output collection
   remain unchanged.
8. Run `cargo test` and confirm the full workspace passes.
9. Run
   `cargo clippy --workspace --all-targets --all-features -- -D warnings` and
   confirm there are no compiler or Clippy warnings.
10. Run the exact end-to-end smoke procedure from the repository root:

    ```bash
    cargo build -p cowboy
    cargo run -p cowboy-agent-acp --bin watchdog-fixture -- verify \
      --cowboy target/debug/cowboy \
      --workspace target/watchdog-smoke \
      --response-timeout-seconds 1 \
      --cancel-timeout-seconds 2 \
      --recovery-operation-timeout-seconds 3 \
      --soft-deadline-seconds 15 \
      --hard-deadline-seconds 20
    ```

    The verifier must create isolated `soft/` and `hard/` state/config/workflow
    directories under `target/watchdog-smoke`. Each generated config must set
    `state_dir` and `workflow_store` inside its scenario directory,
    `workflow_dirs = ["<scenario>/workflows"]`, and one `[[agents]]` entry whose
    command is the current `watchdog-fixture` executable, whose args are
    `["serve", "--mode", "<acknowledge-cancel|ignore-cancel>", "--events", "<scenario>/events.jsonl", "--invocation-token", "<scenario-unique-token>", "--identity-dir", "<scenario>/identities"]`,
    and whose `[agents.watchdog]` values are exactly `1`, `2`, and `3` seconds.
    Each generated `watchdog_smoke.lua` must contain one agent action accepting
    only status `success` with a required string `summary`, then route that result
    to a terminal success status. The fixture's first prompt must stay silent;
    its post-recovery `"Continue"` prompt must emit:

    ```text
    ---
    status: success
    summary: watchdog recovered
    ---
    watchdog recovered
    ```

    The verifier must launch
    `target/debug/cowboy --config <scenario>/config.toml run --workflow watchdog_smoke "watchdog smoke"`,
    and assert these JSONL/log observations:
    - Soft scenario: one `process_started` PID `P1`; one session id `S1`; one
      `cancel_received` for `P1/S1`; one `"Continue"` prompt for `P1/S1`; no
      second `process_started`; Cowboy exits successfully within 15 seconds; log
      events include `agent_watchdog_timeout`, `agent_watchdog_cancel_sent`, and
      `agent_watchdog_soft_recovered`.
    - Hard scenario: initial PID `P2` and session `S2`; one ignored
      `cancel_received`; `P2` is no longer alive before replacement startup;
      replacement PID `P3 != P2` records `resume_session_id == S2`; one
      `"Continue"` prompt uses `P3/S2`; Cowboy exits successfully within 20
      seconds; log events additionally include
      `agent_watchdog_force_terminated` and
      `agent_watchdog_transport_resumed`.
    - Both scenarios: the generated one-step workflow returns status `success`;
      no unrecorded fixture PID remains alive; the verifier removes
      `target/watchdog-smoke` on success.

    If the verifier is interrupted or fails and preserves evidence, run:

    ```bash
    cargo run -p cowboy-agent-acp --bin watchdog-fixture -- cleanup \
      --workspace target/watchdog-smoke
    ```

    Confirm cleanup authenticates each live fixture through its recorded
    loopback endpoint, token, process-start nonce, PID, and canonical executable
    path before requesting self-shutdown. It must exit nonzero, leave the process
    untouched, and preserve evidence if any field mismatches or the endpoint
    cannot prove identity. After all matching fixture processes exit within
    three seconds, it removes only the named smoke workspace.

## TODO

- [x] TODO-01: Define and validate the per-agent watchdog configuration with conservative defaults.
  - Procedure: Add the app and engine watchdog structs/fields, conversion code,
    and config tests, then run `cargo test -p cowboy config::tests`.
  - Expected result: Existing configs parse with 100-second response,
    10-second cancellation, and 30-second recovery-operation defaults; explicit
    values reach `AgentRuntimeConfig`; and zero in any field fails with a
    field-specific message.
  - Observed result: `cargo test -p cowboy config::tests` passed 21 tests,
    covering default `100`/`10`/`30` values, explicit runtime conversion, and
    field-specific rejection of all three zero values.
- [x] TODO-02: Propagate each selected agent's watchdog policy into every production ACP client.
  - Procedure: Add the recording ACP connector described in Changes; route all
    four production construction paths through it; configure deliberately
    different default and named-agent commands/watchdog values; then run:
    1. `cargo test -p cowboy-workflow-engine role_agent_client_construction_uses_explicit_named_watchdog`
    2. `cargo test -p cowboy-workflow-engine request_topic_client_construction_uses_default_watchdog`
    3. `cargo test -p cowboy-workflow-engine workflow_selector_client_construction_uses_default_watchdog`
    4. `cargo test -p cowboy-workflow-engine workflow_improvement_client_construction_uses_default_watchdog`
  - Expected result: Each command invokes its real production path and the
    connector records the exact resolved watchdog policy. The role test records
    the explicit named agent, while request-topic, selector, and summarizer tests
    record the deliberately different default agent, proving named-agent
    resolution remains unchanged.
  - Observed result: All four exact production-construction commands passed.
    The recording connector observed the named role command with watchdog
    `21`/`22`/`23`, while request-topic, workflow-selection, and workflow-
    improvement paths observed the deliberately different default command with
    watchdog `11`/`12`/`13`.
- [x] TODO-03: Add ACP client watchdog options and deterministic reconnect injection for tests.
  - Procedure: Add the default/options constructors, stored policy, and
    replacement-transport factory described in Changes; then run:
    1. `cargo test -p cowboy-agent-acp watchdog_options_retain_defaults_and_explicit_values`
    2. `cargo test -p cowboy-agent-acp replacement_transport_is_consumed_only_after_forced_reconnect`
  - Expected result: Default and explicit policies are retained by the client,
    and the second test records zero replacement-factory calls during normal
    prompting and successful soft recovery, then exactly one call when forced
    reconnect begins and the distinct replacement becomes active.
  - Observed result: Both exact commands passed. Default and explicit options
    were retained, and the deterministic factory remained unused before forced
    reconnect, was called exactly once during hard recovery, and activated the
    distinct replacement transport.
- [x] TODO-04: Implement inactivity detection and ordered soft-cancel recovery in the ACP prompt loop.
  - Procedure: Add the resettable response deadline, fixed cancellation grace
    deadline, cancellation-cause distinction, normal-completion race handling,
    notification-send versus outstanding-prompt-error distinction,
    valid-unrecognized input handling, malformed-input escalation, and
    same-session `"Continue"` dispatch. Add or retain the exact named tests below
    and run each command:
    1. `cargo test -p cowboy-agent-acp watchdog_soft_cancel_continues_same_session`
    2. `cargo test -p cowboy-agent-acp watchdog_soft_parsed_activity_resets_inactivity_deadline`
    3. `cargo test -p cowboy-agent-acp watchdog_soft_normal_completion_wins_ready_timeout_without_cancel`
    4. `cargo test -p cowboy-agent-acp watchdog_soft_prompt_rpc_error_during_cancel_grace_escalates`
    5. `cargo test -p cowboy-agent-acp watchdog_soft_malformed_json_during_cancel_grace_escalates`
    6. `cargo test -p cowboy-agent-acp watchdog_soft_external_cancellation_before_timeout_sends_no_continuation`
    7. `cargo test -p cowboy-agent-acp external_cancellation_during_watchdog_grace_suppresses_continuation`
    8. `cargo test -p cowboy-agent-acp external_cancellation_wins_simultaneous_watchdog_deadline`
  - Expected result: Parsed activity postpones timeout, inactivity emits exactly
    one `session/cancel`, timely cancellation emits exactly one `"Continue"`,
    a matching prompt RPC error or malformed cancel-grace input escalates, and
    external cancellation or a normal completion emits no watchdog continuation;
    simultaneous external cancellation and watchdog expiry selects external
    cancellation. Every test also asserts cancel/continuation counts and that
    soft-only cases do not request or activate a replacement transport.
  - Observed result: All eight exact commands passed. The tests observed
    deadline reset, one cancel and one same-session continuation, completion and
    external-cancellation priority, escalation for prompt RPC and malformed JSON
    failures, and zero replacement-factory calls for soft-only branches.
- [x] TODO-05: Add explicit transport force termination with PID-based stdio process shutdown.
  - Procedure: Extend `Transport`, implement stdio/Zellij/mock/controlled
    transports, then run:
    1. `cargo test -p cowboy-agent-acp force_terminate_stops_stdio_child_by_pid`
    2. `cargo test -p cowboy-agent-acp force_terminate_closes_zellij_pane_once`
    3. `cargo test -p cowboy-agent-acp force_terminate_disposes_mock_transport`
    4. `cargo test -p cowboy-agent-acp force_terminate_disposes_controlled_transport`
  - Expected result: The stdio test's recorded child PID exits after force
    termination, Zellij records exactly one pane-close operation, and both mock
    and controlled transports record that they were force-terminated and
    disposed.
  - Observed result: All four declared force-termination commands passed; the
    stdio child exited by recorded PID, Zellij closed one pane, and mock and
    controlled transports recorded disposal.
- [x] TODO-06: Implement hard restart, session resume, and continuation after cancellation failure or timeout.
  - Procedure: On soft-cancel failure, force-terminate and discard the old
    transport, reconnect with the current session id through
    `create_transport`, initialize it, and send `"Continue"`; apply the configured
    recovery-operation timeout separately to termination, creation,
    initialization, and continuation dispatch; centralize replacement cleanup.
    Add or retain the exact named tests below and run each command:
    1. `cargo test -p cowboy-agent-acp watchdog_hard_restart_resumes_same_session`
    2. `cargo test -p cowboy-agent-acp watchdog_hard_force_termination_error_prevents_replacement`
    3. `cargo test -p cowboy-agent-acp watchdog_hard_force_termination_timeout_prevents_replacement`
    4. `cargo test -p cowboy-agent-acp watchdog_hard_replacement_creation_error_leaves_no_transport`
    5. `cargo test -p cowboy-agent-acp watchdog_hard_replacement_creation_timeout_leaves_no_transport`
    6. `cargo test -p cowboy-agent-acp watchdog_hard_initialization_rpc_error_disposes_replacement`
    7. `cargo test -p cowboy-agent-acp watchdog_hard_initialization_timeout_disposes_replacement`
    8. `cargo test -p cowboy-agent-acp watchdog_hard_continuation_send_error_disposes_replacement`
    9. `cargo test -p cowboy-agent-acp watchdog_hard_continuation_dispatch_timeout_disposes_replacement`
    10. `cargo test -p cowboy-agent-acp watchdog_hard_replacement_rpc_error_disposes_transport`
    11. `cargo test -p cowboy-agent-acp watchdog_hard_replacement_malformed_json_disposes_transport`
    12. `cargo test -p cowboy-agent-acp watchdog_hard_replacement_eof_disposes_transport`
    13. `cargo test -p cowboy-agent-acp watchdog_hard_cleanup_timeout_drops_replacement_transport`
  - Expected result: Tests observe old-transport termination, replacement
    initialization with the same session id, one recovery continuation, and
    explicit bounded errors when terminate, reconnect, initialize, or
    continuation dispatch fails; failed initialization or continuation
    send/protocol/RPC handling leaves no connected replacement transport or live
    replacement child. Error/timeout tests assert phase-specific messages,
    factory-call counts, force-termination/drop counts, and `is_connected() ==
    false`.
  - Observed result: All thirteen exact commands passed. The tests observed
    same-session restart, phase-specific termination/creation/initialization/
    continuation errors and timeouts, exact factory-call counts, forced
    termination and drop of failed replacements, and a disconnected client
    after every failed recovery phase.
- [x] TODO-07: Cover watchdog timing, race, repetition, and existing cancellation regressions.
  - Procedure: Enable Tokio `test-util` in
    `crates/agent/acp/Cargo.toml`; add paused-time controlled-transport tests for
    deadline resets, fixed cancel grace, completion races, repeated stalls,
    notification-write failure, outstanding-prompt RPC error, EOF,
    valid-unrecognized input, malformed JSON in each recovery phase, bounded
    operation timeouts, replacement disposal, external cancellation, and the
    simultaneous external-cancellation/watchdog-deadline race. In addition to
    all exact tests from TODO-04 and TODO-06, add exact tests named
    `watchdog_fixed_cancel_grace_ignores_activity`,
    `watchdog_notification_write_failure_uses_hard_recovery`,
    `watchdog_eof_during_prompt_uses_hard_recovery`,
    `watchdog_eof_during_cancel_grace_uses_hard_recovery`,
    `watchdog_valid_unrecognized_json_does_not_reset_deadline`,
    `watchdog_malformed_json_during_prompt_uses_hard_recovery`,
    `watchdog_second_stall_after_soft_recovery_is_monitored`, and
    `watchdog_second_stall_after_hard_recovery_is_monitored`. Run
    `cargo test -p cowboy-agent-acp client::tests`, then run
    `cargo test -p cowboy-agent-acp external_cancellation_wins_simultaneous_watchdog_deadline`.
  - Expected result: Every recovery branch is deterministic, existing
    cancellation tests still pass, the dedicated simultaneous-ready test proves
    external cancellation wins without watchdog continuation/restart, and no
    branch sends duplicate cancel/continuation messages or leaks an old/failed
    replacement transport. The module command must list and execute every exact
    test named by TODO-04, TODO-06, and this TODO; a passing narrower substring
    filter is not accepted as proof.
  - Observed result: `cargo test -p cowboy-agent-acp client::tests` passed all 62
    module tests and listed every exact TODO-04, TODO-06, and TODO-07 test.
    The dedicated simultaneous-deadline command also passed, with no duplicate
    recovery messages or leaked replacement transport.
- [x] TODO-08: Update configuration and architecture documentation for watchdog behavior.
  - Procedure: Update `README.md`, `demo-config.toml`,
    `docs/architecture.md`, and `docs/module-map.md` with one uniquely marked
    authoritative contract block per Markdown file. Add
    `config::tests::documented_agent_watchdog_contract_is_unique_and_exact` to:
    parse `demo-config.toml` through the real config loader; extract exactly one
    marked block from each Markdown file; compare its normalized field/value and
    ordered recovery text to one expected contract generated from code defaults;
    and reject any watchdog field assignment outside that block or any second,
    missing, or contradictory block. Run
    `cargo test -p cowboy documented_agent_watchdog_contract_is_unique_and_exact`.
  - Expected result: Documentation and the shipped demo show matching field
    names/defaults and required recovery behavior, and the executable consistency
    test's mutation cases prove it fails for a wrong number, swapped recovery
    order, duplicate block, missing block, or contradictory assignment outside
    the authoritative block.
  - Observed result: `cargo test -p cowboy
    documented_agent_watchdog_contract_is_unique_and_exact` passed. It parsed
    `demo-config.toml`, matched one authoritative block in each Markdown file to
    code defaults and ordered recovery text, and rejected all five declared
    mutation cases.
- [x] TODO-09: Run focused regression checks for workflow-agent session and prompt-window behavior.
  - Procedure: Run `cargo test -p cowboy-workflow-agent`.
  - Expected result: Session reuse, prompt watermarks, correction prompts,
    accepted mid-run input cancellation, output parsing, and retry-nudge tests
    pass without watchdog-specific workflow-layer changes.
  - Observed result: `cargo test -p cowboy-workflow-agent` passed 75 tests,
    preserving session reuse, prompt-window cancellation/corrections, output
    parsing, and retry behavior.
- [x] TODO-10: Run full workspace validation and a two-path watchdog smoke test.
  - Procedure: Run `cargo test`, then
    `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
    after TODO-11 is complete, execute the exact `verify` command and, only if
    needed, the exact `cleanup` command from How to verify step 10.
  - Expected result: Workspace tests and Clippy pass; the verifier exits zero
    within the 15/20-second scenario bounds; its JSONL and Cowboy-log assertions
    prove same-PID/same-session soft recovery and old-PID-dead,
    new-PID/different-process, same-session hard resume; authenticated fixture
    self-shutdown leaves no live fixture process, and
    `target/watchdog-smoke` is removed after success.
  - Observed result: `cargo test` and
    `cargo clippy --workspace --all-targets --all-features -- -D warnings`
    passed. The exact verifier command exited zero within its bounds, proved
    soft same-PID/same-session and hard old-PID-dead/new-PID same-session
    recovery, authenticated fixture shutdown, and removed
    `target/watchdog-smoke`; cleanup was not needed.
- [x] TODO-11: Add the deterministic ACP watchdog fixture, self-verifier, and targeted cleanup command.
  - Procedure: Add the `watchdog-fixture` binary and Cargo target with
    `serve`, `verify`, and `cleanup` subcommands exactly as specified in Changes
    and How to verify step 10; add UUID v4 fixture identity support; implement the
    token/start-nonce/PID/canonical-executable challenge-response cleanup
    protocol with refusal on any mismatch; add an injectable verifier scenario
    runner; add the exact lifecycle tests
    `watchdog_fixture_verify_failure_preserves_evidence_workspace` and
    `watchdog_fixture_verify_success_removes_workspace`; then run:
    1. `cargo test -p cowboy-agent-acp watchdog_fixture`
    2. `cargo test -p cowboy-agent-acp watchdog_fixture_verify_failure_preserves_evidence_workspace`
    3. `cargo test -p cowboy-agent-acp watchdog_fixture_verify_success_removes_workspace`
  - Expected result: Fixture tests prove exact initialize/session/prompt/cancel
    handling, `--resume=<session-id>` parsing, JSONL record shape, generated
    config/workflow content, deadline enforcement, full-identity matched
    self-shutdown, refusal to signal a reused/mismatched PID, evidence
    preservation on forced verifier failure, and workspace removal only after a
    fully successful verifier run.
  - Observed result: All three exact commands passed. The 11-test fixture suite
    covered ACP handling, resume parsing, identity and mismatch refusal, and
    generated inputs; the direct failure test preserved its marked evidence
    workspace, while the direct success test removed the workspace only after
    both injected scenarios completed.
