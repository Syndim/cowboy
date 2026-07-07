# Plan

Replace the hand-written TUI slash-command argument parsing in `crates/tui/src/app/commands.rs` with a parser built on existing Rust crates.

Use the `shlex` crate's major version `2` to tokenize the submitted composer line into shell-like argv tokens so quoted arguments and escaped spaces are handled by a maintained crate. Reuse the existing `clap` dependency to validate slash-command arity and shape after tokenization. Keep runtime behavior in `cowboy-workflow-engine` unchanged; this is a TUI parsing/dispatch cleanup only.

Preserve the current command surface:

- `/run <request>` starts a selector-backed run.
- `/run-workflow <workflow-id> <request>` starts a catalog workflow directly.
- `/run-step <request>` starts only the first workflow step.
- `/step <run-id>` executes exactly one further step.
- `/resume [run-id]` resumes the active run when no run id is supplied.
- `/answer <run-id> <prompt-id> <answer>` answers a prompt, with the answer allowed to contain spaces.
- `/improve <run-id>`, `/resolve <run-id> [status] [fields-json]`, `/cancel`, `/runs`, `/workflows`, `/help`, and `/exit` keep their existing meanings.

Parse failures should become non-fatal usage/status cards and must not silently fall through to starting a new workflow run. Plain non-slash text should still start a workflow run, and pending prompt answers should still be routed after explicit slash commands.

# Changes

- Update `crates/tui/Cargo.toml`:
  - add `shlex = "2"` as a `cowboy` crate dependency, or add it to `[workspace.dependencies]` only if another crate also needs it during implementation.
- Update `crates/tui/src/app/commands.rs`:
  - introduce a small typed slash-command parser, for example `ParsedSlashCommand`, driven by `clap` derives;
  - tokenize slash inputs with `shlex` 2.x, treating an unmatched quote as an actionable parse error;
  - normalize the first token from `/command` to `command` before passing argv to `clap`;
  - model trailing text commands with clap settings such as `num_args = 1..`, `trailing_var_arg = true`, and `allow_hyphen_values = true` where needed so requests and answers can start with `-` and can contain spaces;
  - replace `strip_prefix`, `split_once`, and `splitn` command-argument parsing in `dispatch_submitted_input`, `submit_start_workflow`, `submit_explicit_answer`, and `resolve_run` with a match on the typed parsed command;
  - keep the existing background-task spawning helpers and runtime calls so orchestration remains delegated to `WorkflowRuntime`;
  - convert clap/shlex tokenization errors into the existing `state.set_status(...)` plus `state.push_card("Usage", ...)` pattern;
  - keep `SLASH_COMMANDS`, suggestions, completion, and help output aligned with the parser, either by deriving metadata from the parser or by adding a test that every advertised slash command parses.
- Leave `crates/tui/src/main.rs` CLI parsing unchanged except for any harmless shared helper extraction the implementer chooses; the product CLI already uses `clap`.
- Leave `crates/workflow/*` crates unchanged; slash parsing belongs in the TUI crate under the repository responsibility map.
- Update `README.md` only if implementation adds documented user-visible quoting behavior or changes usage wording; otherwise no user-facing command syntax documentation needs to move.

# Tests to be added/updated

- Add focused parser unit tests in `crates/tui/src/app/commands.rs` for:
  - `/run do work` and `/run "do work"` both producing the same request string;
  - `/run-workflow review do work` producing workflow id `review` and request `do work`;
  - `/run-step -- investigate` or an equivalent hyphen-leading request being accepted as request text rather than a clap option;
  - `/resume` producing the no-argument resume variant and `/resume run-1` producing the explicit run id variant;
  - `/answer run-1 prompt-1 answer with spaces` preserving the full answer text;
  - `/answer run-1 prompt-1 "answer with spaces"` preserving the quoted answer text;
  - `/resolve run-1`, `/resolve run-1 approved`, and `/resolve run-1 approved '{"summary":"done"}'` producing the expected typed fields;
  - malformed input such as `/run "unterminated` returning a parse error instead of panicking or dispatching a run;
  - advertised commands in `SLASH_COMMANDS` all parse or intentionally map to metadata-only suggestion behavior.
- Update existing dispatch tests in `crates/tui/src/app/commands.rs`:
  - keep coverage that valid `/run-workflow` dispatch spawns the named-workflow task;
  - keep coverage that missing `/run-workflow` arguments show usage and spawn no task;
  - add or update coverage that missing required arguments for `/run`, `/run-step`, `/step`, `/answer`, `/improve`, and `/resolve` show usage and spawn no task;
  - add coverage that a parser error does not fall through to plain workflow start;
  - keep coverage that plain text still starts a selector-backed run;
  - keep coverage that bare `/resume` uses the active run id when present and shows usage when absent.
- Update suggestion/composer tests only if parser metadata replaces the existing `SLASH_COMMANDS` table.
- No engine, store, Lua, agent, or ACP tests should be required because runtime behavior is unchanged.

# How to verify

- Run `cargo test -p cowboy app::commands` after adding parser and dispatch tests.
- Run `cargo test -p cowboy app::controls::composer` if slash-command metadata or suggestion rendering changes.
- Run `cargo test -p cowboy main::tests` to prove existing product CLI clap parsing was not regressed by any shared parser extraction.
- Run `cargo test -p cowboy` after focused tests pass.
- Manual TUI smoke test:
  - submit `/run "request with spaces"` and confirm a run starts with the dequoted request;
  - submit `/answer <run-id> <prompt-id> "answer with spaces"` against a waiting prompt and confirm the answer is delivered as one value;
  - submit malformed `/run "unterminated` and confirm the TUI shows a usage/error card and does not start a run.

# TODO

- [x] Add the `shlex = "2"` dependency to the `cowboy` crate.
- [x] Define typed `clap` parser types for all supported slash commands in `crates/tui/src/app/commands.rs`.
- [x] Add a `shlex` tokenization helper that reports unmatched quotes as slash parse errors.
- [x] Normalize slash command names before handing argv to `clap`.
- [x] Replace manual `/run-workflow` argument splitting with typed parser output.
- [x] Replace manual `/answer` argument splitting with typed parser output.
- [x] Replace manual `/resolve` argument splitting with typed parser output.
- [x] Replace the top-level `strip_prefix` slash dispatch chain with a match on parsed commands.
- [x] Preserve plain-text workflow submission for non-slash input.
- [x] Preserve pending prompt answer fallback after explicit slash-command handling.
- [x] Convert slash parse and validation errors into non-fatal usage/status cards.
- [x] Keep slash suggestions and help metadata aligned with parser-supported commands.
- [x] Add parser unit tests for quoted, unquoted, trailing, hyphen-leading, missing-argument, and malformed slash inputs.
- [x] Update dispatch tests for valid commands, usage errors, parser errors, plain text, and bare resume behavior.
- [x] Update README command documentation only if user-visible syntax or quoting behavior is documented.
- [x] Run the focused `cowboy` TUI command and parser tests.
- [x] Run the full `cargo test -p cowboy` verification command.
- [x] Preserve `#` as ordinary slash-command payload text instead of shell comments.
- [x] Add regression tests for `#` in `/run`, `/run-workflow`, and `/answer` payloads.
- [x] Re-run the focused `cowboy` TUI command and parser tests after reviewer feedback.
- [x] Re-run the full `cargo test -p cowboy` verification command after reviewer feedback.
- [x] Align `/resolve` slash metadata, usage errors, and README with `[fields-json]` support.
- [x] Re-run focused and full `cowboy` tests after `/resolve` metadata feedback.
