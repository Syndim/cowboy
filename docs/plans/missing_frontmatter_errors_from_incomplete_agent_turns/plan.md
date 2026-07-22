# Plan: Accurate no-result diagnostic for agent replies that carry no workflow result

> Bug-fix work folder: `docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/`
> RCA: [`rca.md`](./rca.md)
> Investigator regression test (input to the fix, **do not rewrite/replace**):
> `crates/workflow/agent/src/executor.rs::executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter`

## Plan

The reviewed RCA establishes that the high volume of `agent response is missing
YAML frontmatter` errors comes from **no-result agent replies** (backend
stall/stream-close notices and nonempty prose/preamble) being funneled into the
single `MissingFrontmatter` diagnostic. The parser
(`parse_frontmatter_output` → `split_frontmatter` → `find_frontmatter_open`,
`crates/workflow/agent/src/frontmatter.rs:112-114`) returns
`Error::MissingFrontmatter` for **any** reply with no opening `---` line. That is
precisely the "reply contained no workflow result" condition, yet the user-visible
diagnostic and the retry `reason`/nudge blame missing frontmatter.

At the provider-neutral `Client::prompt` seam Cowboy sees only
`StopReason::EndTurn` plus text (RCA "The fix contract must be grounded" section);
there is **no structured incomplete-turn signal**. So the grounded, feasible fix
is an **accurate generic diagnostic** — not truncation detection and not
backend-string pattern matching — paired with a **best-effort, side-effect-safe
recovery instruction** appropriate to a reply that carried no workflow result.

**Fix shape (minimal, source-grounded):**

- Keep the parser pure. `parse_frontmatter_output` continues to return
  `Error::MissingFrontmatter` when there is no opening delimiter, so the existing
  parser unit tests and the constructed `MissingFrontmatter` variant are
  unchanged (no dead-code warning).
- Add a distinct `Error::NoWorkflowResult` variant with the accurate message
  `agent reply did not contain a workflow result`, classified `recoverable()`.
- At the executor seam (`crates/workflow/agent/src/executor.rs:609`, the
  `parse_frontmatter_output(&visible)` call) remap **only** the
  `MissingFrontmatter` parse error to `NoWorkflowResult`. A reply that has an
  opening `---` but is malformed still surfaces its precise variant
  (`MissingClosingDelimiter`, `Yaml`, `FrontmatterNotMapping`, `MissingStatus`,
  etc.) — genuine malformed-frontmatter behavior is preserved (RCA "Keep genuine
  malformed-frontmatter behavior").
- Make the retry instruction condition-appropriate and **side-effect-safe**:
  branch `build_retry_nudge` (`prompt.rs:61-78`) so a no-result reason yields a
  best-effort "inspect existing work, continue/complete what remains without
  repeating completed side effects, then return a complete workflow result" nudge
  (see Recovery below), leaving the malformed-frontmatter nudge unchanged.

Because `MissingFrontmatter` is the *only* variant `split_frontmatter` emits for a
missing opening delimiter (`frontmatter.rs:112-114`), remapping exactly that
variant at the executor captures every no-result reply and nothing else.

**Recovery (best-effort, safe for the RCA-established ambiguity).** Relabeling
alone changes the diagnostic but not the retry instruction. The retry nudge
`build_retry_nudge` (`crates/workflow/agent/src/prompt.rs:61-78`) currently says
**"Do not redo the work. Re-emit your result now as a complete replacement…"**,
which assumes a result exists and was only mis-formatted. That instruction is a
poor fit for a reply that carried no parseable result, because per the RCA the
two indistinguishable cases are:

- a stalled/interrupted turn (no work, or partial work), and
- a completed turn whose final prose omitted frontmatter (work already done,
  possibly with side effects: edited files, run commands, commits).

Cowboy **cannot** tell these apart at the `Client::prompt` seam (RCA "The fix
contract must be grounded"), so the retry instruction must be **safe for both**.
It must NOT blanket-instruct "perform the task now" — that could duplicate or
corrupt already-completed side effects for the second case. Instead, for a
no-result reason the nudge is a **best-effort recovery instruction**:

> The previous turn did not produce a parseable workflow result. Inspect the
> existing work and conversation state. Continue or complete any unfinished work
> as needed, without repeating actions or side effects already completed. Then
> return one complete workflow result with valid YAML frontmatter, a valid
> `status`, the required fields, and the Markdown body.

For any other parse/frontmatter reason the existing "re-emit your completed work
with a valid frontmatter block" nudge is unchanged (correct when work exists but
was malformed; RCA established this wording is not defective for that case). Both
branches keep the YAML-frontmatter/`status` requirement (RCA "do not remove the
frontmatter requirement").

**Branch selection uses the production reason string, not a bare message.** The
runner sets the retry `reason` to `last_error.to_string()` (`runner.rs:240,250`),
where `last_error` is a `WorkflowError`; for the new variant that Display is the
**wrapped** form `recoverable action failure: agent reply did not contain a
workflow result` (`WorkflowError::RecoverableAction` Display,
`crates/workflow/core/src/error.rs:39-40`). The executor threads that same string
into `build_retry_nudge` via `context.retry_reason` (`executor.rs:408-412`).
Detection is therefore isolated in a named constant + predicate in `prompt.rs`
(a `const NO_RESULT_REASON_MARKER: &str = "did not contain a workflow result";`
substring check, which matches both the wrapped and bare forms), and is tested
against the **full wrapped runner-style reason**, plus `None` and unrelated
frontmatter reasons.

**Claims kept within the evidence.** The failure stays `recoverable()`, so retry
budget accounting is unchanged; a deliberate back-off or non-recoverable policy
is out of scope (RCA). This is a **best-effort** recovery instruction: the RCA
establishes no evidence that the old delimiter guidance caused the reported
exhaustion, and repeated backend stalls exhaust the budget independently of the
diagnostic — prompt wording cannot recover a persistently dead stream. The plan
therefore does **not** claim the old nudge caused the exhaustion or that the new
wording measurably reduces stalls; it makes the retry instruction accurate and
side-effect-safe for a no-result reply.

## Changes

- `crates/workflow/agent/src/error.rs`
  - Add `Error::NoWorkflowResult` variant:
    `#[error("agent reply did not contain a workflow result")] NoWorkflowResult,`.
  - Add `Error::NoWorkflowResult` to the `true` arm of `recoverable()`
    (`error.rs:48-65`) so `From<Error> for WorkflowError` maps it to
    `RecoverableAction`.
  - Update the `recoverable()` doc comment (`error.rs:42-47`). It currently states
    recoverable parse/frontmatter failures mean "the agent finished its work but
    its final message was malformed" — inaccurate for `NoWorkflowResult`, which
    also covers stall/stream-close/no-result replies that produced no work. Widen
    the wording to cover replies that carried no parseable workflow result.
- `crates/workflow/agent/src/executor.rs`
  - At the `parse_frontmatter_output(&visible)` call site (`executor.rs:609`),
    remap `Err(Error::MissingFrontmatter)` to `Err(Error::NoWorkflowResult)`
    before the existing `.inspect_err(...)`/`?`. All other parse errors pass
    through unchanged. The prompt-window handling around the call is untouched.
- `crates/workflow/agent/src/prompt.rs`
  - Add a named marker constant and predicate for detection, e.g.
    `const NO_RESULT_REASON_MARKER: &str = "did not contain a workflow result";`
    and `fn is_no_result_reason(reason: Option<&str>) -> bool` returning true when
    `reason` contains the marker. The substring matches both the bare
    `Error::NoWorkflowResult` message and the wrapped runner form
    `recoverable action failure: agent reply did not contain a workflow result`.
  - Make `build_retry_nudge` (`prompt.rs:61-78`) branch on `is_no_result_reason`:
    - **no-result reason →** a **best-effort, side-effect-safe** nudge that (i)
      states the previous turn did not produce a parseable workflow result, (ii)
      instructs the agent to inspect existing work/conversation state, (iii)
      continue or complete unfinished work **without repeating actions or side
      effects already completed**, and (iv) return one complete workflow result
      with valid YAML frontmatter, a valid `status`, required fields, and the
      Markdown body. It must NOT say "perform the task now" unconditionally and
      must NOT say "Do not redo the work".
    - **any other reason →** the existing wording unchanged ("Do not redo the
      work. Re-emit your result now as a complete replacement… valid YAML
      frontmatter…").
  - Both branches retain the frontmatter/`status` requirement and, when the action
    allows `blocked`, the appended `BLOCKED_STATUS_POLICY` (so the existing
    `retry_nudge_includes_reason_and_frontmatter_instruction` assertion on
    `BLOCKED_STATUS_POLICY` still holds).

No changes to the parser, the runner, or config/workflow logic. The nudge is
selected from the reason string the runner already threads
(`runner.rs:240,250` → `context.retry_reason` → `build_retry_nudge`,
`executor.rs:408-412`); no new signal is invented.

## Tests to be added/updated

- **Unchanged (investigator regression, input to the fix; must NOT be rewritten or
  weakened):**
  `executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter`
  — passes after the fix ((a) message contains "did not contain a workflow
  result"; (b) not `Error::MissingFrontmatter`; (c) still `recoverable()`).
  Immutability is enforced by a committed pinned baseline of the exact test body,
  `docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/repro_test_baseline.rs.txt`
  (SHA-256 `6ee980c5040187743686fb57080a0d7323af8845ffc3b7acc10979d3bf8497f5`);
  TODO-05 diffs the current source test body against this baseline so a rewritten
  or weakened test is detected as a non-empty diff, not hidden behind a passing
  run.
- **Migrated (required by RCA "Migrate the adjacent plain-prose executor test"):**
  `executor::tests::malformed_final_response_leaves_prompt_window_closed`
  (`crates/workflow/agent/src/executor.rs:2645-2663`) feeds plain
  `"not frontmatter"` prose. Update its classification assertion from
  `Error::MissingFrontmatter` to `Error::NoWorkflowResult`; **preserve** the
  existing prompt-window-closed assertion (`window ... !is_open()`) unchanged.
- **Added:** in `error.rs` tests (`parse_and_transient_errors_are_recoverable`,
  `error.rs:82-114`) add `assert!(Error::NoWorkflowResult.recoverable());`.
- **Added (recovery nudge helper, `prompt.rs` tests):** a new test that calls
  `build_retry_nudge(&action, Some("recoverable action failure: agent reply did
  not contain a workflow result"))` — the **full wrapped runner-style reason** —
  and asserts the result (a) acknowledges no parseable result was received, (b)
  contains the inspect / continue-or-complete-as-needed guidance, (c) contains an
  explicit instruction not to repeat completed actions/side effects, (d) contains
  the complete-workflow-result / `status` / YAML-frontmatter requirement, and (e)
  does NOT contain "Do not redo the work". Also add a `None`-reason case and keep
  the existing `retry_nudge_includes_reason_and_frontmatter_instruction` and
  `retry_nudge_surfaces_precise_closing_delimiter_reason` tests passing (they
  assert the original "Do not redo the work" wording remains for frontmatter
  reasons).
- **Added (production retry-prompt selection, `executor.rs` tests):** a new
  executor test modeled on `retry_attempt_appends_corrective_frontmatter_nudge`
  (`executor.rs:1598-1619`) that sets `context.attempt = 2` and
  `context.retry_reason = Some("recoverable action failure: agent reply did not
  contain a workflow result".to_string())`, runs `execute_agent`, and asserts the
  final prompt (via `execution.record.input.prompt`, or the captured
  `FakeClient.prompt_calls`) selects the no-result branch: it contains the
  inspect/continue/complete-as-needed guidance and the do-not-repeat-side-effects
  protection, and does NOT contain "Do not redo the work". This exercises the real
  runner→executor→prompt path with the wrapped reason, not just the helper.
- **Unchanged parser tests (must keep passing):**
  `frontmatter::tests::rejects_missing_frontmatter` and
  `rejects_prose_without_opening_delimiter` still assert the parser returns
  `Error::MissingFrontmatter` — the parser layer is deliberately untouched.

## How to verify

1. Confirm the investigator regression test is unmodified against the pinned
   baseline, then run it (a passing run alone cannot prove immutability):

   ```bash
   sha256sum docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/repro_test_baseline.rs.txt
   awk '/async fn no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter/{f=1} f{print} f&&/^    }$/{exit}' \
     crates/workflow/agent/src/executor.rs \
     | diff - docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/repro_test_baseline.rs.txt
   cargo test -p cowboy-workflow-agent --lib \
     executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter
   ```

   Expected: the `sha256sum` prints
   `6ee980c5040187743686fb57080a0d7323af8845ffc3b7acc10979d3bf8497f5`; the `diff`
   emits no output and exits 0 (current test body byte-identical to the baseline);
   the test run reports `1 passed`.

2. Run the migrated executor test, the error/parser tests, the retry-nudge helper
   tests, and the production retry-prompt selection test (Cargo takes one
   positional filter per invocation, so run them separately):

   ```bash
   cargo test -p cowboy-workflow-agent --lib malformed_final_response_leaves_prompt_window_closed
   cargo test -p cowboy-workflow-agent --lib parse_and_transient_errors_are_recoverable
   cargo test -p cowboy-workflow-agent --lib rejects_missing_frontmatter
   cargo test -p cowboy-workflow-agent --lib rejects_prose_without_opening_delimiter
   cargo test -p cowboy-workflow-agent --lib retry_nudge
   cargo test -p cowboy-workflow-agent --lib retry_prompt
   ```

   Expected: each command reports its test(s) passing. The `retry_nudge` filter
   runs the `prompt.rs` helper nudge tests (existing frontmatter/closing-delimiter
   plus the new no-result branch); the `retry_prompt` filter runs the new executor
   test that asserts the production retry prompt selects the no-result branch for
   the full wrapped runner-style reason.

3. Run the whole agent crate test suite and lint clean:

   ```bash
   cargo test -p cowboy-workflow-agent
   cargo clippy -p cowboy-workflow-agent --all-targets -- -D warnings
   ```

   Expected: all tests pass; no compiler or Clippy warnings (in particular, no
   "variant never constructed" for either `MissingFrontmatter` or
   `NoWorkflowResult`).

The immutable RCA evidence artifacts in this folder
(`source_log_manifest.json`, `extract_genuine_emissions.py`,
`aggregate_frontmatter_failures.py`, `emit_evidence_excerpts.py`,
`trace_run_lifecycle.py`, and their committed outputs) are the investigation
record, not implementation verification. They depend on the private
`<state_dir>/logs` and are unaffected by this code change, so re-running them is
**out of scope** for verifying the fix.

## TODO

- [x] TODO-01: Add `Error::NoWorkflowResult` variant and make it recoverable.
  - Procedure (ordered):
    1. In `crates/workflow/agent/src/error.rs`, add the variant
       `#[error("agent reply did not contain a workflow result")] NoWorkflowResult,`
       to the `Error` enum.
    2. In the same file, add `Error::NoWorkflowResult` to the `true` match arm of
       `recoverable()`.
    3. Inspect the edited source and confirm both edits are present (the variant in
       the enum and the entry in the `true` arm).
    4. Run `cargo build -p cowboy-workflow-agent`.
  - Expected result: step 3 shows the variant and the `true`-arm entry present;
    step 4 compiles with no warnings; `Error::NoWorkflowResult.recoverable()`
    returns `true`.
  - Observed result: step 3 inspection shows `#[error("agent reply did not contain
    a workflow result")] NoWorkflowResult,` in the `Error` enum and
    `| Error::NoWorkflowResult` in the `recoverable()` `true` arm; step 4
    `cargo build -p cowboy-workflow-agent` finished clean (no warnings). Matches.

- [x] TODO-02: Remap the no-opening-delimiter parse failure to `NoWorkflowResult`
  at the executor seam.
  - Procedure (ordered):
    1. In `crates/workflow/agent/src/executor.rs` at the
       `parse_frontmatter_output(&visible)` call (line ~609), insert a mapping that
       converts `Err(Error::MissingFrontmatter)` to `Err(Error::NoWorkflowResult)`
       (e.g. `.map_err(|err| match err { Error::MissingFrontmatter => Error::NoWorkflowResult, other => other })`)
       before the existing `.inspect_err(...)` and `?`.
    2. Inspect the edited call site and confirm only `MissingFrontmatter` is
       remapped and every other variant passes through (`other => other`).
    3. Run `cargo build -p cowboy-workflow-agent`.
  - Expected result: step 2 shows the sole `MissingFrontmatter`→`NoWorkflowResult`
    remap with all other variants untouched; step 3 compiles cleanly; a
    no-frontmatter reply now yields `Error::NoWorkflowResult` while replies with an
    opening `---` still yield their precise malformed-frontmatter variants.
  - Observed result: step 2 inspection shows the call site chains
    `.map_err(|err| match err { Error::MissingFrontmatter => Error::NoWorkflowResult,
    other => other })` before `.inspect_err`/`?` — only `MissingFrontmatter` is
    remapped, `other => other` passes every other variant through; step 3
    `cargo build -p cowboy-workflow-agent` clean. Matches.

- [x] TODO-03: Migrate the adjacent plain-prose executor test while preserving its
  prompt-window assertion.
  - Procedure (ordered):
    1. In `crates/workflow/agent/src/executor.rs`, in
       `malformed_final_response_leaves_prompt_window_closed`, change the assertion
       `assert!(matches!(error, Error::MissingFrontmatter));` to
       `assert!(matches!(error, Error::NoWorkflowResult));`.
    2. Inspect the test and confirm the following `window ... !is_open()` assertion
       is unchanged.
    3. Run `cargo test -p cowboy-workflow-agent --lib malformed_final_response_leaves_prompt_window_closed`.
  - Expected result: step 2 shows the classification assertion migrated to
    `NoWorkflowResult` with the prompt-window-closed assertion intact; step 3 →
    `test result: ok. 1 passed`.
  - Observed result: step 2 inspection shows the assertion is
    `assert!(matches!(error, Error::NoWorkflowResult));` and the following
    `window ... !is_open()` assertion is unchanged; step 3 →
    `test result: ok. 1 passed`. Matches.

- [x] TODO-04: Add the recoverability assertion for the new variant.
  - Procedure (ordered):
    1. In `crates/workflow/agent/src/error.rs` test
       `parse_and_transient_errors_are_recoverable`, add
       `assert!(Error::NoWorkflowResult.recoverable());`.
    2. Inspect the test and confirm the new assertion is present.
    3. Run `cargo test -p cowboy-workflow-agent --lib parse_and_transient_errors_are_recoverable`.
  - Expected result: step 2 shows the assertion present; step 3 →
    `test result: ok. 1 passed`.
  - Observed result: step 2 inspection shows
    `assert!(Error::NoWorkflowResult.recoverable());` present in
    `parse_and_transient_errors_are_recoverable`; step 3 →
    `test result: ok. 1 passed`. Matches.

- [x] TODO-05: Confirm the investigator regression test is byte-for-byte unchanged
  against the pinned baseline, then passes.
  - Procedure (ordered):
    1. Do **not** edit the investigator test
       `no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter`.
    2. Verify the committed baseline fixture is intact by hash — it is the pinned
       immutable snapshot of the test body:
       `sha256sum docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/repro_test_baseline.rs.txt`
       and confirm it equals
       `6ee980c5040187743686fb57080a0d7323af8845ffc3b7acc10979d3bf8497f5`.
    3. Extract the current test body from source and compare it to that baseline,
       exact bytes:
       `awk '/async fn no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter/{f=1} f{print} f&&/^    }$/{exit}' crates/workflow/agent/src/executor.rs | diff - docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/repro_test_baseline.rs.txt`.
    4. Run `cargo test -p cowboy-workflow-agent --lib executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter`.
  - Expected result: step 2 prints the pinned SHA-256 exactly (baseline
    unmodified); step 3 `diff` produces **no output and exits 0** (current test body
    is byte-identical to the pinned baseline, so a rewritten or weakened test is
    detected as a diff); step 4 → `test result: ok. 1 passed`. All three together —
    not the test run alone — prove the investigator test is both unmodified and
    passing.
  - Observed result: step 2 `sha256sum repro_test_baseline.rs.txt` printed
    `6ee980c5040187743686fb57080a0d7323af8845ffc3b7acc10979d3bf8497f5` (baseline
    intact); step 3 `awk … | diff - baseline` produced no output and exited 0
    (current test body byte-identical); step 4 →
    `test result: ok. 1 passed`. Matches.

- [x] TODO-06: Full crate test + lint gate.
  - Procedure (ordered):
    1. Run `cargo test -p cowboy-workflow-agent`.
    2. Run `cargo clippy -p cowboy-workflow-agent --all-targets -- -D warnings`.
  - Expected result: all tests pass; zero compiler and Clippy warnings (including
    no "variant never constructed" warning for `MissingFrontmatter` or
    `NoWorkflowResult`).
  - Observed result: step 1 `cargo test -p cowboy-workflow-agent` → 54 lib + 2 bin
    tests passed, 0 failed; step 2 `cargo clippy … --all-targets -- -D warnings`
    → Finished with no warnings (no variant-never-constructed warning). Matches.

- [x] TODO-07: Update the `recoverable()` doc comment to cover no-result replies.
  - Procedure (ordered):
    1. In `crates/workflow/agent/src/error.rs`, edit the doc comment above
       `pub fn recoverable(&self)` (`error.rs:42-47`) so it no longer claims all
       recoverable parse/frontmatter failures mean the agent finished its work with
       a malformed final message; widen it to also cover a reply that carried no
       parseable workflow result (`NoWorkflowResult`, e.g. a stall/stream-close or
       no-result reply).
    2. Inspect the revised comment and confirm it describes both the
       malformed-final-message case and the no-result case.
    3. Run `cargo build -p cowboy-workflow-agent`.
  - Expected result: step 2 shows the widened wording covering both cases; step 3
    builds with no warnings.
  - Observed result: step 2 inspection shows the doc comment now reads "Parse/
    frontmatter failures mean the agent's reply carried no parseable workflow
    result: either it finished its work but its final message was malformed, or the
    reply contained no workflow result at all (a stall/stream-close or no-result
    reply)"; step 3 `cargo build -p cowboy-workflow-agent` clean. Matches.

- [x] TODO-08: Add a no-result reason predicate and branch `build_retry_nudge` to
  a best-effort, side-effect-safe recovery nudge.
  - Procedure (ordered):
    1. In `crates/workflow/agent/src/prompt.rs`, add
       `const NO_RESULT_REASON_MARKER: &str = "did not contain a workflow result";`
       and `fn is_no_result_reason(reason: Option<&str>) -> bool` (true when
       `reason` contains the marker; matches both the bare and the wrapped
       `recoverable action failure: …` forms).
    2. Branch `build_retry_nudge` (`prompt.rs:61-78`) on `is_no_result_reason`: for
       a no-result reason emit a nudge that (i) states the previous turn did not
       produce a parseable workflow result, (ii) tells the agent to inspect
       existing work/conversation state, (iii) continue or complete unfinished work
       **without repeating actions or side effects already completed**, and (iv)
       return one complete workflow result with valid YAML frontmatter, a valid
       `status`, required fields, and Markdown body. It must NOT contain "perform
       the task now" unconditionally and must NOT contain "Do not redo the work".
       Keep the current wording verbatim for all other reasons.
    3. Inspect the branch and confirm: the no-result branch has the four elements
       above and omits "Do not redo the work"; the frontmatter branch wording is
       unchanged; and both branches keep the
       `build_output_instruction`/`BLOCKED_STATUS_POLICY` append for actions that
       allow `blocked`.
    4. Run `cargo build -p cowboy-workflow-agent`.
  - Expected result: step 3 shows the side-effect-safe no-result branch, the
    unchanged frontmatter branch, and the preserved blocked-policy append; step 4
    compiles cleanly.
  - Observed result: step 3 inspection shows `NO_RESULT_REASON_MARKER` +
    `is_no_result_reason`; the no-result branch states "did not produce a parseable
    workflow result", instructs "Inspect the existing work", "Continue or complete
    any unfinished work", "without repeating actions or side effects already
    completed", requires "one complete workflow result" with `status`/opening/
    closing `---`, and omits "Do not redo the work"; the frontmatter branch keeps
    the original "Do not redo the work…" wording; both retain the
    `build_output_instruction`/`BLOCKED_STATUS_POLICY` append; step 4 build clean. Matches.

- [x] TODO-09: Add a helper regression test for the no-result nudge using the full
  wrapped runner-style reason.
  - Procedure (ordered):
    1. In `crates/workflow/agent/src/prompt.rs` tests, add a test (named to match
       the `retry_nudge` filter) that calls
       `build_retry_nudge(&action, Some("recoverable action failure: agent reply did not contain a workflow result"))`
       and asserts the returned string (a) acknowledges no parseable result was
       received, (b) contains the inspect / continue-or-complete-as-needed
       guidance, (c) contains the do-not-repeat-completed-side-effects instruction,
       (d) contains the complete workflow-result / `status` / YAML-frontmatter
       requirement, and (e) does NOT contain "Do not redo the work".
    2. Add a `None`-reason assertion that the original frontmatter wording is used.
    3. Inspect the added test(s) and confirm cases (a)–(e) and the `None` case are
       all asserted against the wrapped reason.
    4. Run `cargo test -p cowboy-workflow-agent --lib retry_nudge`.
  - Expected result: step 3 shows all invariants asserted; step 4 passes the new
    test(s) alongside the existing
    `retry_nudge_includes_reason_and_frontmatter_instruction` and
    `retry_nudge_surfaces_precise_closing_delimiter_reason` tests (which still
    assert the "Do not redo the work" wording for frontmatter reasons).
  - Observed result: step 3 inspection shows
    `retry_nudge_no_result_reason_is_side_effect_safe` asserting (a)–(e) against the
    wrapped reason and `retry_nudge_none_reason_uses_frontmatter_wording` for the
    `None` case; step 4 `cargo test … --lib retry_nudge` → `test result: ok. 4
    passed` (2 new + 2 existing frontmatter tests). Matches.

- [x] TODO-10: Add an executor production-path test that the real retry prompt
  selects the no-result branch for the wrapped reason.
  - Procedure (ordered):
    1. In `crates/workflow/agent/src/executor.rs` tests, add a test (named to match
       the `retry_prompt` filter), modeled on
       `retry_attempt_appends_corrective_frontmatter_nudge` (`executor.rs:1598-1619`):
       set `context.attempt = 2` and `context.retry_reason = Some("recoverable action failure: agent reply did not contain a workflow result".to_string())`,
       call `execute_agent`, and assert the final prompt (via
       `execution.record.input.prompt`, or the captured `FakeClient.prompt_calls`)
       contains the inspect/continue/complete-as-needed guidance and the
       do-not-repeat-completed-side-effects protection, and does NOT contain "Do not
       redo the work".
    2. Inspect the test and confirm the attempt/wrapped-reason setup and all prompt
       assertions are present.
    3. Run `cargo test -p cowboy-workflow-agent --lib retry_prompt`.
  - Expected result: step 2 shows the wrapped-reason setup and the prompt
    assertions; step 3 passes, proving the runner→executor→`build_retry_nudge` path
    selects the no-result branch for the full wrapped reason.
  - Observed result: step 2 inspection shows
    `retry_prompt_selects_no_result_branch_for_no_result_reason` sets
    `context.attempt = 2` and the wrapped `context.retry_reason`, runs
    `execute_agent`, and asserts the prompt contains the inspect/continue/complete
    guidance and the do-not-repeat-side-effects protection and NOT "Do not redo the
    work"; step 3 `cargo test … --lib retry_prompt` → `test result: ok. 1 passed`. Matches.
