# Plan

Revise the in-progress resolve input design from a single `name=value` token to a repeatable two-token field pair shared by the CLI and TUI:

- `cowboy resolve <run-id> <status> [--field <name> <value>]... [--body <text>]`
- `/resolve <run-id> <status> [--field <name> <value>]... [--body <text>]`

Separating the name from the value removes the delimiter ambiguity without restricting workflow output schemas. Pass each field name through exactly as entered—do not trim, normalize, or split it—so names containing `=`, names beginning with `-`, names with surrounding whitespace, and empty names continue to match the arbitrary UTF-8 string keys currently accepted by Lua output-field conversion. Configure the fixed two-value clap option to accept hyphen-leading values on both shared command surfaces. Generated commands must quote the name and value as separate tokens so every required field shown by `resolution_options` round-trips through both a POSIX shell and Cowboy's slash tokenizer.

Keep the existing value behavior: an ordinary right-hand token becomes a JSON string, while a valid JSON literal preserves its type. Common input remains concise (`--field summary "manual resolution"`), and structured values remain available (`--field retry false`, `--field files '["src/a.rs"]'`). Reject duplicate names by exact string equality and malformed values that look like structured JSON. Keep the existing requirement that fields and body may only be supplied with a status.

This is a clean correction to the in-progress command format: remove the ambiguous single-token `--field <name=value>` grammar rather than supporting two competing field syntaxes. Keep `--body` and `WorkflowRuntime::resolve_run(..., Option<serde_json::Value>, ...)` unchanged; the command-parser crate continues to assemble field pairs into the JSON object expected by the runtime.

# Changes

- Update `crates/tui/command-parser/src/lib.rs`:
  - replace `ResolveFieldAssignment::from_str` and `ResolveArgs.fields: Vec<ResolveFieldAssignment>` with a repeatable clap option that consumes exactly two values per occurrence, displayed as `--field <name> <value>`;
  - set the field option to accept hyphen-leading names and values, while retaining `requires = "status"` and shared CLI/slash parsing;
  - pair the collected values in `resolve_fields_object`, preserve each name byte-for-byte as a Rust string, parse only the value using the established string/JSON rules, and retain exact duplicate-name rejection;
  - do not reject or trim empty, all-whitespace, surrounding-whitespace, `=`-containing, or leading-hyphen names because workflow loading currently accepts those keys;
  - update generated CLI help, slash help, validation usage, and suggestions from `<name=value>` to the two-value shape.
- Update `crates/tui/app/src/resolution.rs` so `resolution_command` emits `--field '<exact name>' '...'` for each required field. Keep the existing POSIX/slash-compatible quoting helper, but test that the revised token boundaries preserve the reviewed field-name edge cases.
- Update `crates/tui/app/src/main.rs` and `crates/tui/app/src/app/commands.rs` for the parser helper's revised field-pair input, while preserving runtime dispatch, duplicate-field error surfacing, malformed-value usage cards, and status/body behavior.
- Update `crates/workflow/engine/src/runtime.rs` so missing-field guidance uses the same separate name/value command form. Do not change exact-key validation, `ResolutionStatus.required_fields`, or resolution routing.
- Update the existing CLI integration workflow and TUI resolve smoke coverage to require and submit field names containing `=`, a leading hyphen, and surrounding whitespace, proving the generated commands satisfy the runtime's exact-key lookup rather than only parser-level assertions.
- Update `README.md`, `docs/architecture.md`, and the repository `AGENTS.md` command reference and examples to use `--field <name> <value>` and explain that field names are exact while values retain the current plain-string/JSON-literal conversion.

# Tests to be added/updated

- Update `cowboy-command-parser` tests for identical CLI and slash parsing of zero, one, and repeated `--field <name> <value>` pairs, quoted spaces, values containing `=`, retained JSON types, malformed structured values, exact duplicate names, and fields/body without a status.
- Add focused parser boundary tests proving names `foo=bar`, `-review`, ` review `, and the empty string are preserved exactly in the assembled JSON object. Include leading-hyphen values and names that resemble options so clap cannot reinterpret either token as another flag.
- Update clap help and generated slash metadata assertions to advertise `--field <name> <value>` and reject the removed single-token `--field name=value`, legacy `--fields`, and positional JSON forms.
- Extend `crates/tui/app/src/resolution.rs` round-trip tests so generated commands containing the reviewed boundary names decode to the exact names and placeholder values through the slash tokenizer and, on Unix, through a POSIX shell.
- Update `cowboy` CLI integration coverage with a failed workflow whose required fields include `foo=bar`, `-review`, and ` review `; list options, execute the emitted command shape with real values, and assert the run completes with those exact fields visible to the next step.
- Update the TUI resolve smoke test with the same exact-key boundaries, asserting the Resolve card renders copyable separate-token commands and slash dispatch forwards the exact assembled object without starting a plain-text run.
- Update engine guidance tests to assert `--field '<exact name>' '...'` for punctuation, leading-hyphen, and surrounding-whitespace names while retaining existing required-field and `ctx.prev` routing coverage.

# How to verify

- Run `cargo test -p cowboy-command-parser resolve` for the revised grammar, exact-name boundaries, typed values, removed syntax, generated help, and validation errors.
- Run `cargo test -p cowboy resolution` and `cargo test -p cowboy --test resolve_cli` for command rendering plus CLI/TUI dispatch through workflows requiring `=`, leading-hyphen, and surrounding-whitespace field names.
- Run `cargo test -p cowboy` after the focused app checks to cover shared slash usage and command behavior.
- Run `cargo test -p cowboy-workflow-engine resolution` and `cargo test -p cowboy-workflow-engine resolve_run` to confirm guidance, exact required-field lookup, and routing remain correct.
- Run Clippy with warnings denied for `cowboy-command-parser`, `cowboy`, and `cowboy-workflow-engine` across all targets.
- Smoke-test a deliberately failed workflow from both interfaces: list resolution options, copy the generated command for required boundary names, replace each `...` value, and confirm the run advances with exact keys and preserved string/boolean/array types.
- Confirm `--field summary=value`, legacy `--fields`, and positional JSON are rejected with generated usage pointing to `--field <name> <value>`.

# TODO

- [x] Add `serde_json` support and a parser-owned resolve field representation.
- [x] Implement plain-string and JSON-literal value conversion with actionable, redacted malformed-JSON errors.
- [x] Assemble repeated fields into a JSON object with exact duplicate-name rejection.
- [x] Require a resolve status whenever fields or body are supplied.
- [x] Migrate CLI and TUI dispatch away from whole-object JSON parsing.
- [x] Preserve `--body` and `WorkflowRuntime::resolve_run` behavior.
- [x] Add ordinary-name coverage for typed string, boolean, number, array, object, and null values.
- [x] Add actionable duplicate-field and redacted malformed-value handling in CLI and TUI paths.
- [x] Replace the ambiguous single-token `name=value` parser with repeatable two-token field pairs.
- [x] Preserve field names exactly without splitting, trimming, normalizing, or blank-name rejection.
- [x] Accept leading-hyphen field names and values in both CLI and slash parsing.
- [x] Update field-object assembly for the revised parser representation while retaining exact duplicate detection and typed values.
- [x] Update generated CLI/slash help, usage, and suggestions to `--field <name> <value>`.
- [x] Update CLI and TUI resolve dispatch for the revised field-pair helper interface.
- [x] Render copyable per-status resolve commands with separately quoted exact names and placeholder values.
- [x] Update runtime missing-field guidance to the same lossless separate-token syntax.
- [x] Add parser boundary tests for equals-sign, leading-hyphen, surrounding-whitespace, empty, and option-like field names.
- [x] Add parser tests for leading-hyphen values and rejection of the removed single-token syntax.
- [x] Extend command-rendering round-trip tests through the slash tokenizer and POSIX shell for boundary names.
- [x] Extend CLI integration coverage with a workflow requiring boundary field names and exact next-step lookup.
- [x] Extend TUI integration coverage for copyable boundary-name commands and exact field forwarding.
- [x] Update engine guidance assertions for separately quoted boundary field names.
- [x] Update `README.md`, `docs/architecture.md`, and `AGENTS.md` syntax and examples.
- [x] Run the focused parser, app, CLI integration, and engine resolution tests.
- [x] Run the full `cowboy` crate tests after focused checks pass.
- [x] Run warnings-denied Clippy for every touched crate and target.
- [x] Perform CLI and TUI failed-run smoke tests with boundary names and typed values.
