# RCA: High volume of "missing YAML frontmatter" errors from no-result agent replies funneled into one misleading diagnostic

> Reported run handle: **`ca3f4e0a`** (durable run id generalized to `<run-id>`).
> Additional user direction: *"figure out why we have that much errors about
> missing frontmatter and fix that issue"* — so this RCA addresses the **systemic
> error volume**, of which run `<run-id>` is one instance.

> **Clarification (answering "are we parsing a partial response that doesn't
> include the frontmatter?").** Yes — Cowboy passes the accumulated reply text to
> the YAML-frontmatter parser (`parse_frontmatter_output` on the `visible` reply),
> finds no `---` block, and reports `MissingFrontmatter` ("agent response is
> missing YAML frontmatter"). What that text was, staying strictly within the
> frozen evidence:
>
> - **8 replies — unambiguous no-result backend notices:** 7 stall notices
>   ("Anthropic stream stalled…") and 1 stream-close notice ("OpenAI responses
>   stream closed…"). These are clearly not a workflow result.
> - **3 replies — nonempty prose/preamble without frontmatter or a recognized
>   notice:** the retained evidence **cannot** tell whether these turns were cut
>   short (truncated preamble) or an ordinary prose answer that simply omitted the
>   required frontmatter. This RCA does not claim they were truncated.
> - **All 11:** no parseable workflow result was present in the reply.
>
> The `MissingFrontmatter` label is misleading because the real condition is "the
> reply contained no workflow result," and — critically — at the provider-neutral
> `Client::prompt` seam Cowboy receives only `StopReason::EndTurn` plus text, with
> **no signal** distinguishing a stalled/cut-short turn from a model that answered
> without frontmatter. Because those conditions are indistinguishable there, the
> fix is an accurate **generic "reply did not contain a workflow result"
> diagnostic**, not a claim that Cowboy detected truncation. The retries then keep
> re-asking for frontmatter, which a no-result turn never supplies, so the budget
> is exhausted.

## Bug behavior

Runs fail with the per-step retry budget exhausted, the last recoverable error
being `agent response is missing YAML frontmatter`:

```
invalid action: config set "default" exhausted retry budget for step "plan":
2/2 retries used; last recoverable error: recoverable action failure: agent
response is missing YAML frontmatter
```

`MissingFrontmatter` (`Error` Display: "agent response is missing YAML
frontmatter") recurs far more often than genuinely malformed frontmatter would
explain. Extracting the **genuine** executor emissions of `agent step: failed to
parse frontmatter output` from the diagnostic logs (log→fixture extractor
`extract_genuine_emissions.py`, matched by exact source-location prefix so the
phrase inside agent tool payloads is excluded) and classifying reply shape
deterministically (`aggregate_frontmatter_failures.py`):

- **11 genuine parse failures across 5 distinct runs**, on steps `plan`,
  `review_rca`, and `implement`.
- **`parser_recoverable_frontmatter_count = 0`** — computed, not asserted: none
  carried a `---` delimiter or a ```` ```yaml ```` fence that parser-side recovery
  could rescue.
- Reply shapes (narrowed, honest classification):
  - **8 unambiguous backend notices** — 7 `backend_stall_notice`
    (`"Anthropic stream stalled while waiting for the next event"`) and 1
    `backend_stream_closed_notice`
    (`"OpenAI responses stream closed before a terminal response event was
    received"`). These are clearly no-result replies.
  - **3 ambiguous `incomplete_preamble_or_prose`** — nonempty replies with no
    delimiter and no recognized notice. From their bytes alone Cowboy **cannot**
    distinguish a truncated turn from an ordinary model response that simply
    omitted frontmatter. This RCA does **not** claim these three are incomplete
    turns; it claims only that they carried no parseable workflow result.
  - `empty_or_whitespace` — 0 observed.

Common thread across all 11: the agent reply **contained no parseable workflow
result**, yet every one was reported as `MissingFrontmatter` and retried until
the budget was exhausted.

## Root cause

The executor parses the visible reply **unconditionally**. In
`crates/workflow/agent/src/executor.rs`, `run_prompt_turn` returns
`(visible, turns, stop_reason)` (executor.rs:876-893) and the caller immediately
calls `parse_frontmatter_output(&visible)` (executor.rs:609). Via
`find_frontmatter_open` (`crates/workflow/agent/src/frontmatter.rs:266-277`), any
reply lacking a `---`-followed-by-newline collapses to the single
`Error::MissingFrontmatter` (`crates/workflow/agent/src/frontmatter.rs:113`,
variant at `crates/workflow/agent/src/error.rs:15-16`). That variant is then:

1. surfaced with the message "agent response is missing YAML frontmatter",
   inaccurate for a reply that carried **no workflow result** (an unambiguous
   stall/stream-close notice, or nonempty prose/preamble with no parseable
   result); and
2. classified recoverable (`Error::recoverable()`,
   `crates/workflow/agent/src/error.rs:48-65`) and retried on the reused session.
   The retry nudge `build_retry_nudge`
   (`crates/workflow/agent/src/prompt.rs:61-78`) **already asks for a complete
   replacement** result ("Do not redo the work. Re-emit your result now as a
   complete replacement with a valid YAML frontmatter block…"), so the nudge
   wording is not itself the established defect. The defect is the **misleading
   classification**: the failure is attributed to missing frontmatter rather than
   to a reply that contained no workflow result, and that inaccurate reason is
   surfaced to the user and fed back as the retry `reason`
   (`crates/workflow/engine/src/runner.rs:240,250`). Because the reply carried no
   workflow result — an unambiguous stall/stream-close notice in the 8 notice
   cases, and nonempty prose/preamble with no parseable result in the other 3
   (which the evidence cannot classify as truncated vs. frontmatter-less) — the
   retries reproduce the same no-result failure and exhaust the budget
   (`crates/workflow/engine/src/runner.rs:281-284`).

### Worked example: the reported run traced end to end (run `ca3f4e0a`, step `plan`)

This is the exact reported run. Its full step lifecycle is in the frozen manifest
log `cowboy.2026-07-21.1631015.log` (SHA-256 pinned in `source_log_manifest.json`);
the committed `run_lifecycle.txt` reconstructs it deterministically via
`trace_run_lifecycle.py --run ca3f4e0a` (see Reproduction step D), emitting the
source basename + line number and the parsed sanitized fields for every event.
Sanitized lifecycle (line numbers are the frozen-log lines in the artifact):

```text
L182    START plan (visit 1)
L9341   reply plan reply_chars=5442            (parsed OK — no parse failure)
L9342   START review_plan (visit 1)            <- intervening different-step visit
L36020  reply review_plan reply_chars=7355     (parsed OK — no parse failure)
L36021  START plan (visit 2)                    <- workflow routed back to plan
L54214  reply plan reply_chars=2054
L54215  PARSE-FAIL plan  shape=prose/preamble (ambiguous)   [visit 2, initial attempt]
L54223  START plan  retry  step_retries_used=1  (immediately follows L54215, same step)
L61537  reply plan reply_chars=57
L61538  PARSE-FAIL plan  shape=stall-notice                 [visit 2, retry 1]
L61539  START plan  retry  step_retries_used=2  (immediately follows L61538, same step)
L67863  reply plan reply_chars=57
L67864  PARSE-FAIL plan  shape=stall-notice                 [visit 2, retry 2]
L67865  RUN-ERROR  kind=retry-exhaustion step=plan counters=2/2   -> run Failed (2026-07-21 12:35)
--- next day: a separate /resume of the retained Failed step ---
L67905  LOADED-RUN status=Failed current_step=plan steps_executed=3
L67906  RESUME    status=Failed current_step=plan
L67909  START plan (visit 3)
L76618  reply plan reply_chars=57
L76619  PARSE-FAIL plan  shape=stall-notice                 [visit 3, re-exhausts]
L76620  RUN-ERROR  kind=retry-exhaustion step=plan counters=2/2
```

All fields above are parsed from the log by the generator (the `RUN-ERROR`
`kind=retry-exhaustion step=plan counters=2/2` is extracted from the actual error
reason; `LOADED-RUN`/`RESUME` `status`/`current_step` are parsed from those
lines), not synthesized. The `review_plan` visit at L9342–L36020 is emitted as
concrete evidence that a **different-step visit** separates plan visit 1 from plan
visit 2 — which is why L36021 is a new visit-start, not a retry. (The tracing log
proves the `review_plan` visit and the return to `plan`; it does **not** record
`review_plan`'s routing status, so this RCA does not assert *why* it routed back,
only that it did.)

**Retry accounting (reconciling the four parse failures with `2/2`).** Retry
counters are durable and cumulative per step id (`step_retries_used`,
`crates/workflow/engine/src/runner.rs:226-231`). The initial attempt of a visit
does **not** consume a retry slot; only re-dispatches after a recoverable failure
do (`retry_step`, runner.rs:221-258). For the reported exhaustion:

- **visit 2, attempt 1** (parse-fail at frozen log line 54215, an *ambiguous
  prose/preamble* reply, `reply_chars=2054`) is the visit's initial attempt — it
  does not increment `step_retries_used`.
- **visit 2, attempt 2** (parse-fail at line 61538, *stall notice*,
  `reply_chars=57`) is the **first retry dispatch** → `step_retries_used = 1/2`.
- **visit 2, attempt 3** (parse-fail at line 67864, *stall notice*,
  `reply_chars=57`) is the **second retry dispatch** → `step_retries_used = 2/2`,
  which trips `retry_exhaustion_error` (runner.rs:280-284) → `RunFailed`
  (2026-07-21 12:35, runner.rs:169-183).

So the two dispatches that consumed the two durable retry slots are visit-2
attempts 2 and 3. The **fourth** parse failure (line 76619, next day
2026-07-22 03:11) is not part of that exhaustion at all: it is a **separate
`/resume` of the already-`Failed` run** (`runtime.rs:778` "resuming non-terminal
run; re-executing the retained current step"), i.e. visit 3's initial attempt,
which re-exhausts immediately. The reported run's budget was therefore exhausted
by **one ambiguous prose/preamble failure plus two stall failures**, not "four
identical stalls."

**ACP→parser flow for one failing turn (visit 2, attempt 2, session `<redacted>`).**
The three lines 61536/61537/61538 are consecutive and share `session_id`+`run_id`
(this is the correlation the committed `evidence_excerpts.txt` emits):

1. **Backend turn ends.** `run_prompt_turn` (`executor.rs:876-893`) calls
   `Client::prompt`; the ACP client logs `ACP prompt turn completed
   session_id=<redacted> id=4 stop_reason=EndTurn activity=Text trailing_text=false`
   (frozen log line 61536). The only visible text was the notice `"Anthropic
   stream stalled while waiting for the next event"` — no workflow result — but
   because it is a nonempty `MessageChunk`, ACP classes the turn
   `PromptTurnActivity::Text` and returns `StopReason::EndTurn`
   (`crates/agent/acp/src/client.rs:85,105-107,668`). `PromptTurnActivity` is
   private to the ACP crate and never crosses `Client::prompt`, so the executor
   gets no signal that the turn produced no result.
2. **Executor records the reply.** `executor.rs:508` logs `agent step: initial
   reply … session_id=<redacted> stop_reason=EndTurn reply_chars=57` (frozen log
   line 61537) — `reply_chars=57` is exactly the length of the stall notice, the
   `visible` string about to be parsed.
3. **Parser runs on the no-result text.** `execute_agent` calls
   `parse_frontmatter_output(&visible)` unconditionally (`executor.rs:609`);
   `split_frontmatter`→`find_frontmatter_open`
   (`crates/workflow/agent/src/frontmatter.rs:266-277`) finds no `---` followed by
   a newline, so it returns `Err(Error::MissingFrontmatter)` (`frontmatter.rs:112-114`).
4. **Surfaced as MissingFrontmatter.** The `inspect_err` at `executor.rs:609-616`
   logs the parse failure (frozen log line 61538); `Error::MissingFrontmatter`
   maps to `WorkflowError::RecoverableAction` (`crates/workflow/agent/src/error.rs`),
   Display "agent response is missing YAML frontmatter".
5. **Runner retries with that reason.** `recoverable()` is true
   (`error.rs:48-65`); the runner increments the counters (runner.rs:226-231),
   emits `StepRetrying { reason: last_error.to_string() }` (runner.rs:234-242) and
   re-dispatches with the reason fed into `build_retry_nudge` (executor.rs:408-415).

**The defect vs. the exhaustion cause (separated).** There are two distinct
things here:

- **The established defect (diagnostic only).** At step 3–4 a reply that carried
  **no workflow result** is classified as `MissingFrontmatter`. This produces an
  inaccurate user-visible diagnostic and an inaccurate retry `reason`/nudge. This
  is what the fix corrects.
- **The exhaustion cause (independent of the label).** The run failed because the
  same step produced **repeated recoverable no-result turns**, and recoverable
  failures consume the retry budget by design. The causal chain is:

  ```text
  repeated no-result turns
    → each is a recoverable failure that consumes a retry slot
    → after max_retries_per_step (2) the step exhausts → run fails

  (separately) the current classifier
    → labels each no-result failure inaccurately as MissingFrontmatter
  ```

  Correcting the classification makes the diagnostic and retry reason accurate;
  it does **not** by itself prevent this run from failing, because two more
  no-result turns would still exhaust the (still-recoverable) budget. Whether
  no-result turns should be retried differently (e.g. distinct back-off or a
  non-recoverable classification) is a separate policy decision, explicitly out of
  scope for the approved classification-only fix (see Fix constraints).

### The fix contract must be grounded — no invented signal (corrected ACP flow)

The provider-neutral `Client::prompt` (`crates/agent/client/src/traits.rs:107-113`)
returns only `anyhow::Result<StopReason>`; streamed text arrives through the
`event_handler`. The ACP client's `PromptTurnActivity`
(`crates/agent/acp/src/client.rs:74-108`) is **private to `cowboy-agent-acp`** and
fully consumed inside `prompt()` before returning — it does **not** cross the
`Client` boundary and never reaches `AgentExecutor`. A stall/stream-close notice
is delivered as an `Event::MessageChunk` (text), so the ACP turn is classified
`PromptTurnActivity::Text` (client.rs:85), `should_continue()` is false
(client.rs:105-107), and the turn returns `StopReason::EndTurn` (client.rs:668).
The logs confirm every failing turn is `ACP prompt turn completed
stop_reason=EndTurn activity=Text`. ACP's genuinely-empty-`EndTurn` handling
(auto-`"Continue"` up to 5×, then an `anyhow` client error, client.rs:494-544)
does not apply, because these replies are nonempty `Text`.

**Consequence for the fix:** at the seam the executor actually observes, a
no-result reply is `StopReason::EndTurn` + text with no `---` — indistinguishable
from an ordinary completed prose reply that omitted frontmatter. There is **no
structured incomplete-turn signal** available, and extending `Client` cannot
manufacture information the ACP protocol/backend does not provide. The feasible,
grounded contract is therefore **not** "detect the incomplete turn" but **an
accurate generic diagnostic** for a reply that contained no workflow result, plus
a retry nudge appropriate to that generic case (ask for a complete result, not
merely re-delimiting). The three ambiguous prose/preamble records are covered by
exactly this generic framing.

### Notice-string origin (narrowed to exact-literal presence)

An exact-literal search over `crates/` for `stream stalled` / `stalled while
waiting` / `stream closed before a terminal` / `terminal response event` finds
**no product-code author** of these strings; the only in-repo occurrence is the
literal embedded in this investigation's own regression-test fixture. This
establishes only **exact-literal absence in product source** — it does not by
itself attribute the strings to a specific provider (that would require
transport-level evidence). The takeaway for the fix: do not pattern-match these
brittle, non-product strings; use the generic no-result classification.

## Reproduction steps

### A. Deterministic in-repo regression (executor/client seam)

1. Build/test the agent crate from the repository root.
2. Drive `AgentExecutor::execute_agent` (through the provider-neutral `Client`
   via the test `FakeClient`) with a single `Event::MessageChunk` whose text is
   the observed backend stall notice — a representative no-result reply.
3. Observe the executor returns `Err(Error::MissingFrontmatter)`, whose message is
   "agent response is missing YAML frontmatter" — the inaccurate diagnostic for a
   reply that carried no workflow result.

Captured as the automated regression test below.

### B. Systemic frequency evidence (log→fixture→summary, all executable)

Committed artifacts in this folder, forming the full chain:

- `source_log_manifest.json` (SHA-256
  `e50dac3a6c1c22d3df196f880017ac0dbe26ba3328b39c1e4606c656ff772b21`) — the
  **frozen source-log manifest**: the ordered, generalized basenames and SHA-256
  of exactly the 5 diagnostic log files (11 total emissions) used for this
  investigation. This pins the source population so later log growth cannot
  silently change the fixture.
- `extract_genuine_emissions.py` (SHA-256
  `f0e9273f92cda94aa031619ee88b23082e34a87e8ecfbce00e39d363e225f292`) — consumes
  the manifest (a **frozen file list**, not a live directory glob), verifies each
  file's SHA-256 before reading (exits non-zero on any missing file or hash
  mismatch), matches genuine emissions by exact source-location prefix, and
  **computes** every classification field (`rendered_reply_len`,
  `has_frontmatter_delim`, `has_yaml_fence`, `matched_backend_notice`) from the
  **tracing-rendered `reply=` field** (not the original raw bytes; `tracing`
  renders the string inline with embedded newlines shown as the literal escape
  `\n`, so `rendered_reply_len` is the rendered-field length), then redacts the
  reply prose. Nothing is investigator-asserted.
- `genuine_emissions.sanitized.json` (SHA-256
  `de29736c8eac2b3786feddf421e2be6aba3b914183c9251f3d8b8a1725c5e5a4`) — the
  extractor's sanitized output (11 records; no reply prose, no absolute paths, no
  full run UUID).
- `aggregate_frontmatter_failures.py` (SHA-256
  `7b95436ff98c803d198ba94e1c08f8e9719ccc7630d568e00df2700ec85e49b2`) — the
  deterministic classifier/aggregator (ordered rules in its docstring; only
  `has_opening_delim`/`yaml_code_fence` are parser-recoverable).
- `missing_frontmatter_frequency.json` (SHA-256
  `361b756160dc086efd566f3d9ba67bf81eaa82cd0e1907762591fdd192ec5f5c`) — the
  summary.

Reproduce the log→fixture transform (manifest-based; requires read access to the
private logs, but the manifest hash-verifies the frozen file set so the output is
byte-stable regardless of later log growth):

```bash
python3 docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/extract_genuine_emissions.py \
  --manifest docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/source_log_manifest.json \
  --logs-dir <state_dir>/logs \
  | diff - docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/genuine_emissions.sanitized.json
```

Reproduce the fixture→summary transform (committed artifacts only):

```bash
python3 docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/aggregate_frontmatter_failures.py \
  docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/genuine_emissions.sanitized.json \
  | diff - docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/missing_frontmatter_frequency.json
```

Both expected: exit 0, no differences. The private logs named in the manifest live
under `<state_dir>/logs` and are source-labeled investigator evidence, not
committed; the extractor exits non-zero if any manifest file is absent or its
SHA-256 no longer matches, so a later mismatch is attributable (missing/changed
source), never a silent drift.

### C. Direct log evidence (answering "do we have logs indicating this case?")

**Yes.** The committed `evidence_excerpts.txt` (regenerated deterministically by
`emit_evidence_excerpts.py` from the same frozen, hash-verified manifest logs)
quotes the actual tracing lines, **correlated per turn**. Command:

```bash
python3 docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/emit_evidence_excerpts.py \
  --manifest docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/source_log_manifest.json \
  --logs-dir <state_dir>/logs \
  | diff - docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/evidence_excerpts.txt
```

**Correlation model (defensible, code-grounded).** Within one step turn the
executor logs, in order in one log file: (1) `ACP prompt turn completed …
session_id=<S> stop_reason=… activity=…`; (2) `agent step: initial reply …
session_id=<S> stop_reason=… reply_chars=<N>` (executor.rs:508, **same
`session_id` S**, immediately after the turn; `correction reply` on a retry); and
(3) on parse failure, `agent step: failed to parse frontmatter output run_id=<R>
step=<T> reply=<…>` (executor.rs:610). The emitter joins each parse-failure to the
**nearest preceding executor reply-debug line with the same `run_id`+`step`**
(yielding `session_id` S and `reply_chars` N), and that reply-debug line to the
**nearest preceding ACP completion line carrying the same `session_id` S`** — a
`session_id`-plus-adjacency join, not a bare same-file sample. All 11 emissions
correlate.

Representative correlated turns (basenames only; run/session ids and reply prose
redacted). The parse failure is emitted from `executor.rs:610` — the exact
`parse_frontmatter_output(&visible)` call site — on the accumulated reply text,
and its correlated ACP line shows the turn returned `StopReason::EndTurn` with
`activity=Text` (so at the provider-neutral seam it looks like an ordinary text
reply), while the reply-debug line's `reply_chars` ties the length to the parsed
text:

```text
# Unambiguous no-result backend notice (stall) — correlated turn:
ACP prompt turn completed session_id=<redacted> id=4 stop_reason=EndTurn activity=Text trailing_text=false
agent step: initial reply run_id=<redacted> step=plan session_id=<redacted> stop_reason=EndTurn reply_chars=57
executor.rs:610: agent step: failed to parse frontmatter output run_id=<redacted> step=plan reply=Anthropic stream stalled while waiting for the next event

# Unambiguous no-result backend notice (stream close) — correlated turn:
ACP prompt turn completed session_id=<redacted> id=8 stop_reason=EndTurn activity=Text trailing_text=false
agent step: initial reply run_id=<redacted> step=review_rca session_id=<redacted> stop_reason=EndTurn reply_chars=76
executor.rs:610: agent step: failed to parse frontmatter output run_id=<redacted> step=review_rca reply=OpenAI responses stream closed before a terminal response event was received

# Nonempty prose/preamble without frontmatter (cause undetermined) — correlated turn:
ACP prompt turn completed session_id=<redacted> id=3 stop_reason=EndTurn activity=Text trailing_text=false
agent step: initial reply run_id=<redacted> step=plan session_id=<redacted> stop_reason=EndTurn reply_chars=2054
executor.rs:610: agent step: failed to parse frontmatter output run_id=<redacted> step=plan reply=The reviewer requires a replan. Let me i… <prose redacted>
```

For the 57-character stall notice, the correlated reply-debug line records
`stop_reason=EndTurn reply_chars=57` — a nonempty `EndTurn` text turn that
nonetheless carried no `---` block. Together these correlated triples show, from
the real logs, that the frontmatter parser is run on replies that contain no
workflow result (unambiguously so in the 8 backend-notice cases), that those turns
completed at the ACP layer as `EndTurn`/`Text`, and that the failure is then
labeled `MissingFrontmatter`.

### D. Reproducible retry-accounting trace (manifest-backed)

The worked-example lifecycle and retry accounting above are reproduced
deterministically by `trace_run_lifecycle.py` into the committed
`run_lifecycle.txt`:

```bash
python3 docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/trace_run_lifecycle.py \
  --manifest docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/source_log_manifest.json \
  --logs-dir <state_dir>/logs --run ca3f4e0a \
  | diff - docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/run_lifecycle.txt
```

Expected: exit 0. The generator hash-verifies the manifest logs and emits, in
file order with source basename + line number for every event: **all** step
starts (including the intervening `review_plan` visit, so the retry-vs-new-visit
distinction is checkable), each reply's length, each parse failure and its
sanitized shape, and the runtime `RUN-ERROR`/`LOADED-RUN`/`RESUME` lines with
their fields **parsed from the log** (error `kind`/step/counters, run `status`,
`current_step`) — nothing hard-coded. A same-step re-dispatch is labeled `retry`
only when it immediately follows a same-step parse failure with no intervening
different-step start, and the emitted row states that proof condition. Durable
`StepRetrying` records live in the mutable `events/<run>.json`, not in these
frozen tracing logs, so retry consumption is derived from this adjacency
(runner.rs:221-231); the generator does not fabricate `StepRetrying` lines.

## Regression test

- **Test file path:** `crates/workflow/agent/src/executor.rs`
- **Test name:** `executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter`
- **Command:**

  ```bash
  cargo test -p cowboy-workflow-agent --lib \
    executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter
  ```

- **Expected result before the fix:** the test **fails**. Driving a no-result
  reply through the real `execute_agent`/`Client::prompt` seam, the test asserts
  three observable contract points: (a) **positive** — the user-visible
  diagnostic contains "did not contain a workflow result", the intended accurate
  generic message, so an unrelated recoverable error does not satisfy it; (b) the
  error is classified **distinctly** from `Error::MissingFrontmatter` (asserted on
  the variant, so a mere message rewording of that variant is rejected); and (c)
  the error stays `recoverable()`. Before the fix the executor returns
  `MissingFrontmatter` (message "agent response is missing YAML frontmatter"), so
  (a) fails immediately. The test intentionally makes **no** assertion about the
  retry-nudge wording: the existing nudge already requests a complete replacement
  result, and this investigation has no evidence that its delimiter guidance
  caused non-convergence, so the nudge is out of scope for this contract.

This exercises the executor/client seam responsible for the error volume and
asserts the exact intended generic diagnostic plus a distinct classification — a
global wording change to `MissingFrontmatter` cannot satisfy the positive
message assertion or the variant assertion.

## Current failing result

```
running 1 test
test executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter ... FAILED

---- executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter stdout ----
thread 'executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter'
panicked at crates/workflow/agent/src/executor.rs:2712:9:
a nonempty reply carrying no parseable result must be diagnosed as "agent reply did not
contain a workflow result", got: agent response is missing YAML frontmatter (MissingFrontmatter)

failures:
    executor::tests::no_result_reply_gets_accurate_no_result_diagnostic_not_missing_frontmatter

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 50 filtered out; finished in 0.00s
```

The panic confirms the root cause: a no-result reply is diagnosed with the
inaccurate `MissingFrontmatter` message instead of an accurate no-result
diagnostic.

## Fix constraints

- **Tests/docs only during investigation.** No product code was changed to
  produce this RCA and the failing regression test.
- **Grounded contract: accurate generic no-result diagnosis, not an invented
  incomplete-turn signal.** Because no structured incomplete-turn signal crosses
  the provider-neutral `Client` boundary, classify a reply that produced no
  parseable workflow result with an accurate generic error/category (e.g.
  `NoWorkflowResult` / "agent reply did not contain a workflow result") distinct
  from `MissingFrontmatter`. Do not claim Cowboy knows the turn was incomplete,
  and do not pattern-match backend notice strings.
- **Retry nudge is not established as defective; leave it or improve it, but do
  not remove the frontmatter requirement.** `build_retry_nudge`
  (`crates/workflow/agent/src/prompt.rs:61-78`) already requests a **complete
  replacement** result ("Do not redo the work. Re-emit your result now as a
  complete replacement with a valid YAML frontmatter block…"), and this
  investigation has **no** evidence that its opening/closing `---` guidance caused
  the retries to fail to converge. A later turn must still produce valid
  frontmatter, so the delimiter requirement is reasonable to keep. If the fix
  chooses to tailor the nudge to the no-result reason, it must remain a positive
  request for a complete workflow result with a valid `status` and the required
  frontmatter — not a weaker or empty nudge. This is optional; the established
  defect is the classification, not the nudge wording.
- **Preserve recoverability unless a policy change is chosen deliberately.** The
  regression asserts the no-result error stays `recoverable()`; any retry/back-off
  policy change is a deliberate fix-step decision.
- **Keep genuine malformed-frontmatter behavior.** A reply that *does* contain a
  malformed/partial frontmatter block may still surface its precise
  frontmatter/YAML/schema error; only content-free/no-result replies get the new
  generic classification.
- **Migrate the adjacent plain-prose executor test.**
  `executor::tests::malformed_final_response_leaves_prompt_window_closed`
  (`crates/workflow/agent/src/executor.rs`) currently feeds plain `"not
  frontmatter"` prose and asserts `Error::MissingFrontmatter`. Under the generic
  executor-level no-result classification, that plain-prose reply carries no
  workflow result and must receive the **new no-result category** instead. The
  fix must update that test's classification expectation accordingly **while
  preserving its existing prompt-window-closure assertion** (the window is left
  closed on failure). Do not delete or weaken that assertion.
- **Data safety.** Keep the run id generalized to `<run-id>`/handle `ca3f4e0a`,
  absolute state paths as `<state_dir>`, and reply prose sanitized. Committed
  fixtures/scripts emit only sanitized structural data.
- **Delivery.** Commit locally only; do not push or open a PR.
