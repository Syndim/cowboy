# Plan: Recover frontmatter with an omitted closing delimiter

## Plan

This plan implements the fix for the bug analyzed in
[`rca.md`](./rca.md) in this same work folder. The reviewed and user-approved
RCA is the source of truth for root cause; this document turns it into concrete,
repository-grounded work items.

> **Revision note (replan).** An initial implementation was committed locally
> (`4d9df98 fix(agent): recover frontmatter with an omitted closing delimiter`).
> Post-implementation review found two defects that require changing the
> **documented recovery algorithm** and the **strict-path scope constraint**, so
> this plan is revised and the implementation must be updated to match. A
> replacement/amended local commit is acceptable; do not push or open a PR.

**Problem (from RCA).** Agent replies frequently contain a well-formed opening
YAML frontmatter delimiter (`---`), a complete and valid set of fields
(including `status`), and a Markdown body, but omit the **closing** `---`
delimiter. `split_frontmatter` in `crates/workflow/agent/src/frontmatter.rs`
requires both delimiters and returns `Error::MissingFrontmatter` for *both* the
"no opening delimiter" and "no closing delimiter" cases. Consequences: no
recovery for an omitted closing delimiter; the lenient parser
(`parse_lenient_frontmatter`) never runs because it is reached only *after*
`split_frontmatter` succeeds; the retry nudge tells the model to make its reply
*begin* with frontmatter (which it already does), so the retry reproduces the
same output; and completed, otherwise-valid work cannot serialize into a
`StepRecord`.

**Two defects found in the first implementation (must be fixed by this revision).**

1. **Colon-shaped body absorbed as an invented field.** The first
   implementation's boundary rule ("the body begins at the first top-level,
   non-blank line that is not a valid field line") means a top-level
   colon-shaped body line such as `Note: verification passed` parses as a field
   (`parse_lenient_field("Note: verification passed") = Some(("Note", "verification passed"))`)
   and is pulled into the recovered YAML mapping. Body content must not become
   fields.

2. **Loose closing-delimiter recognition.** `split_frontmatter` locates the
   closing delimiter with `after_open.find("\n---")`, which also matches
   `\n---text` and `\n----`. So `---text`, `----`, or any line beginning with
   `---` (including inside a block scalar or body) can prematurely terminate the
   frontmatter block.

**Existing repro test (do not rewrite).** The investigator added the failing
regression test
`crates/workflow/agent/src/frontmatter.rs::recovers_frontmatter_with_omitted_closing_delimiter`.
It stays as-is and is an input to this work; it must remain green after the
revised fix, without weakening any assertion.

### Fix strategy

1. Keep the two-variant error classification: `MissingFrontmatter` = no opening
   delimiter present (plain prose); `MissingClosingDelimiter` = opening present
   but no valid closing `---`. Both stay recoverable. Precise `Display` strings
   feed the retry nudge and diagnostics.
2. **Harden closing-delimiter recognition (strict path scope change).** Replace
   the `find("\n---")` substring scan with a line-based scan that recognizes a
   closing delimiter **only** when a line is exactly `---` (a trailing `\r` is
   allowed for CRLF) and is terminated by `\n`/`\r\n` or EOF. `---text`, `----`,
   and `--- ` (any trailing content) must **not** close. This same standalone-`---`
   recognition is used everywhere the strict split scans for the closing
   delimiter, and it also prevents premature closing when a body or block-scalar
   line begins with `---`. This intentionally amends the earlier "preserve the
   strict path unchanged" constraint.
3. **Deterministic blank-line/body-boundary recovery.** When an unambiguous
   opening delimiter is present but no standalone `---` closing line exists,
   recover the YAML/body split deterministically using a blank-line boundary that
   is block-scalar aware (see the algorithm below). Recovery is accepted **only**
   when the recovered YAML region parses as a valid YAML mapping (via the
   existing strict-then-lenient parser) and yields a valid string `status`. Plain
   prose, malformed/ambiguous YAML, and missing `status` are rejected with their
   precise errors.
4. Keep the reworded `build_retry_nudge` so the generic instruction is correct
   for opening/closing/status failures and surfaces the precise reason.
5. Preserve any `user_feedback` frontmatter field verbatim through recovery (no
   rewrite/merge/reorder/append).
6. Add regression tests (below) and run the focused suite, clippy, and
   `cargo test --workspace`. Commit locally only.

### Conservative recovery boundary algorithm (deterministic, blank-line based)

Given `after_open` (the text immediately after the opening `---`, with leading
CR/LF stripped), scan line by line and split the YAML region from the Markdown
body as follows.

State: `saw_field: bool`, `in_array: bool` (the last top-level field opened a
block sequence), `in_block_scalar: bool` (the last top-level field opened a `|`
or `>` block scalar).

For each line (top-level means starts at column 0, i.e. not a space/tab):

1. **Inside an active block scalar** (`in_block_scalar == true`):
   - A **blank line** stays in the block scalar (it does **not** end the YAML
     region). This is the critical carve-out: block scalars may contain blank
     lines.
   - An **indented** line (starts with a space or tab) stays in the block
     scalar.
   - A top-level, non-blank line **ends** the block scalar; clear
     `in_block_scalar` and reprocess this line under the rules below.
2. **Blank line** (not inside a block scalar): this is the **body boundary**. The
   YAML region ends here; the body is everything after the blank-line
   separation. Stop scanning.
3. **Indented line** (starts with a space/tab): an indented continuation of the
   preceding field (nested mapping value, wrapped scalar, or indented list
   item). Stays in the YAML region.
4. **Top-level list item** (`- ` or a bare `-`) while `in_array == true`: a
   column-0 block-sequence item belonging to the preceding `key:`. Stays in the
   YAML region.
5. **Top-level field line** (`parse_lenient_field` accepts it — `key:` or
   `key: value` with key in `[A-Za-z0-9_\-$]`): stays in the YAML region. Set
   `saw_field = true`. Determine continuation state from the value:
   - value is empty → `in_array = true` (a block sequence or nested mapping may
     follow), `in_block_scalar = false`;
   - value is a block-scalar indicator (`|` or `>`, optionally followed only by
     chomping/indent indicators such as `-`, `+`, or digits) → `in_block_scalar =
     true`, `in_array = false`;
   - otherwise → `in_array = false`, `in_block_scalar = false`.
6. **Any other top-level, non-blank line** (not a field, not a list item of an
   open array): the **body boundary**. The YAML region ends here; the body starts
   at this line. Stop scanning. This handles a Markdown heading (`## Plan`) or
   prose that immediately follows the fields with no blank-line separation.

After scanning: if `saw_field == false`, the region is plain prose → return
`Error::MissingClosingDelimiter` (do not fabricate a mapping). Otherwise the
YAML region is the text before the boundary and the body is the text after it
(leading blank line(s) trimmed).

**Key behavioral consequence.** Because rule 2 (blank line) fires **before** any
colon-shaped line after the separation is examined, a blank-line-separated
top-level colon-shaped body line such as `Note: verification passed` remains
**body** and is never added to `fields`. Block scalars (rule 1) and top-level
list items (rule 4) still keep their blank lines / items inside the YAML region.
A colon-shaped line that is **not** blank-line separated from the fields (no
intervening blank line) is genuinely ambiguous and is still treated as a field;
this is the conservative, deterministic trade-off and is documented here so the
plan and implementation do not diverge.

The recovered YAML region is then validated by the existing
`parse_frontmatter_mapping` (strict `serde_yaml`, then the existing lenient
fallback) and the `status` extraction in `parse_frontmatter_output`, so
malformed/ambiguous YAML and missing/typed `status` still fail with precise
errors.

### Related parser/error/prompt/diagnostic paths reviewed (same-defect sweep)

- `crates/workflow/agent/src/frontmatter.rs` — `split_frontmatter` (hardened
  closing scan + recovery entry), `recover_unclosed_frontmatter` (rewritten
  boundary), the new standalone-`---` line predicate, `parse_frontmatter_output`,
  `parse_frontmatter_mapping`, `parse_lenient_frontmatter`.
- `crates/workflow/agent/src/error.rs` — `MissingFrontmatter` vs
  `MissingClosingDelimiter`; keep all frontmatter/parse errors `recoverable()`.
- `crates/workflow/agent/src/prompt.rs` — `build_retry_nudge` reason wiring and
  wording.
- `crates/workflow/agent/src/executor.rs` — parse call site and the
  `failed to parse frontmatter output` log; confirm the precise error type flows
  to the retry reason. The existing `matches!(error, Error::MissingFrontmatter)`
  assertion uses plain prose (no opening delimiter) and must keep matching the
  opening-missing variant.
- `crates/workflow/engine/src/runner.rs` — reason plumbing
  (`last_error.to_string()` → `StepRetrying.reason` / `retry_current_step`); no
  logic change needed beyond benefiting from precise `Display` strings.

## Changes

- **`crates/workflow/agent/src/error.rs`**
  - Keep `MissingFrontmatter` = "no opening delimiter present" (plain prose) with
    its existing message so plain-prose tests/behavior are unchanged.
  - Keep `MissingClosingDelimiter` with a precise `Display` message (e.g.
    `agent response has an opening \`---\` but is missing the closing \`---\` delimiter`)
    and include it in `recoverable()`.

- **`crates/workflow/agent/src/frontmatter.rs`**
  - Add a standalone-closing-delimiter predicate, e.g.
    `is_closing_delimiter_line(line)` that strips a single trailing `\n` then a
    single trailing `\r` and returns `content == "---"` (so `---text`, `----`,
    and `--- ` are rejected).
  - Replace the `after_open.find("\n---")` scan in `split_frontmatter` with a
    line-based scan (`split_inclusive('\n')`) that finds the first line satisfying
    `is_closing_delimiter_line`, using it as the strict close. The YAML region is
    the text before that line; the body is the text after that line, trimmed.
    When no such line exists, call `recover_unclosed_frontmatter`.
  - Rewrite `recover_unclosed_frontmatter` to the deterministic blank-line/
    body-boundary algorithm above, with `in_block_scalar` and `in_array` state,
    the block-scalar blank-line carve-out, top-level list-item handling, and the
    non-field top-level line fallback. Return `Error::MissingClosingDelimiter`
    when no field line is present.
  - Add a small helper to detect a block-scalar indicator value (`|`/`>` plus
    optional `-`/`+`/digits) used when setting `in_block_scalar`.
  - Keep routing the recovered YAML region through the existing
    `parse_frontmatter_mapping` + `status` validation so malformed/ambiguous YAML
    and missing `status` fail with precise errors. Preserve `user_feedback`
    verbatim (fields pass through untouched; locked by a test).

- **`crates/workflow/agent/src/prompt.rs`**
  - Keep `build_retry_nudge` worded so the generic instruction covers the opening
    `---`, the fields/`status`, and the closing `---`, and surfaces the precise
    reason in parentheses.

- **`crates/workflow/agent/src/executor.rs`** (review only unless needed)
  - Confirm the precise error type propagates through `?` → `From<Error>` →
    `WorkflowError::RecoverableAction(to_string())` into the retry reason. The
    plain-prose `matches!(..., Error::MissingFrontmatter)` assertion stays valid.

- **`crates/workflow/engine/src/runner.rs`** (review only)
  - No logic change expected; it already forwards `last_error.to_string()` as the
    nudge reason.

## Tests to be added/updated

Keep the investigator repro test
`recovers_frontmatter_with_omitted_closing_delimiter` **unchanged** as a
regression guard; it must pass after the revised fix.

- **`crates/workflow/agent/src/frontmatter.rs` — new/updated positive cases**
  - **Blank-line-separated colon-shaped body stays body (defect 1).** Opening
    `---`, valid fields incl. `status`, a blank line, then `Note: verification passed`
    (and more prose), no closing `---` → recovered; assert
    `parsed.output.fields.get("Note").is_none()` (or the mapping has no such key)
    and that the body contains `Note: verification passed`.
  - **Block scalar containing a blank line is preserved** under the blank-line
    boundary: a `|` (or `>`) block scalar whose indented content includes a blank
    line, followed by a blank-line-separated Markdown body, no closing `---` →
    recovered; the block scalar value retains its internal blank line and the body
    starts at the heading/prose.
  - **Top-level list preserved:** `key:` followed by column-0 `- ` items, then a
    blank-line-separated body, no closing `---` → recovered; list items in fields,
    body separate.
  - **`---text` and `----` do not close (defect 2):** frontmatter whose region or
    body contains a line `---text` and a separate case with `----`, no standalone
    `---` → the loose match must not terminate the block; assert the expected
    recovery (fields/status recovered, the `---text`/`----` line treated as body)
    or the appropriate precise error — not a mangled early close.
  - **Standalone `---` still closes for LF and CRLF and at EOF:** three strict
    cases — a normal LF `---` close, a CRLF (`---\r\n`) close, and a `---` as the
    final line at EOF (no trailing newline) — all parse via the strict path.
  - **Markdown heading/prose body** after fields with no closing `---` (existing)
    → recovered.
  - **Prose preamble before the opening delimiter** with an omitted closing `---`
    (mirrors `parses_frontmatter_after_agent_preamble`) → recovered.
  - **Valid closed block** still parses via the strict path (recovery code does
    not alter fully-delimited behavior).
  - **`user_feedback` preservation:** a recovered block with a `user_feedback`
    list is carried through verbatim (same order, same items, nothing appended).
- **`crates/workflow/agent/src/frontmatter.rs` — new/updated negative cases**
  - Plain prose, no opening delimiter → `Error::MissingFrontmatter`.
  - Opening delimiter present, prose-only region (no field lines), no closing
    `---` → `Error::MissingClosingDelimiter`.
  - Opening delimiter + valid-looking fields but **no `status`**, no closing
    `---` → `Error::MissingStatus`.
  - Opening delimiter + malformed/ambiguous YAML with no closing `---` →
    `Error::Yaml`/`Error::FrontmatterNotMapping`.
- **`crates/workflow/agent/src/error.rs`**
  - Assert `Error::MissingClosingDelimiter.recoverable()` and that its
    `WorkflowError` conversion is `RecoverableAction`.
- **`crates/workflow/agent/src/prompt.rs`**
  - Nudge test asserting the reworded generic instruction and that a
    closing-delimiter reason string is surfaced (e.g. nudge contains
    "closing `---`" when given that reason).

## How to verify

Run from the repository root.

1. Focused repro (must pass after the fix):

   ```bash
   cargo test -p cowboy-workflow-agent --lib \
     frontmatter::tests::recovers_frontmatter_with_omitted_closing_delimiter
   ```

2. Focused frontmatter + error + prompt suites:

   ```bash
   cargo test -p cowboy-workflow-agent --lib frontmatter
   cargo test -p cowboy-workflow-agent --lib error
   cargo test -p cowboy-workflow-agent --lib prompt
   ```

3. Lint the touched crate and fix all warnings before finishing:

   ```bash
   cargo clippy -p cowboy-workflow-agent --all-targets
   ```

4. Full workspace suite:

   ```bash
   cargo test --workspace
   ```

5. Confirm the change set and commit **locally only** (no push, no PR). A
   replacement/amended commit on top of / in place of `4d9df98` is acceptable:

   ```bash
   git --no-pager diff --stat
   git --no-pager log --oneline -1
   ```

## TODO

- [x] Keep `Error::MissingFrontmatter` = "no opening delimiter present" and `Error::MissingClosingDelimiter` = "opening present, closing missing" in `crates/workflow/agent/src/error.rs`; keep both `recoverable()` and mapping to `RecoverableAction`.
- [x] Add `is_closing_delimiter_line` (strip one trailing `\n` then one `\r`, compare `== "---"`) in `crates/workflow/agent/src/frontmatter.rs` so `---text`, `----`, and `--- ` do not close.
- [x] Replace the `after_open.find("\n---")` scan in `split_frontmatter` with a line-based scan using `is_closing_delimiter_line`; standalone `---` closes for LF, CRLF, and EOF; otherwise fall through to recovery.
- [x] Rewrite `recover_unclosed_frontmatter` to the deterministic blank-line/body-boundary algorithm: block-scalar-aware blank-line carve-out, top-level list items of an open array, indented continuations, non-field top-level line fallback, and `MissingClosingDelimiter` when no field line is present.
- [x] Add the block-scalar indicator detection helper (`|`/`>` + optional `-`/`+`/digits) used to set `in_block_scalar`.
- [x] Ensure a blank-line-separated top-level colon-shaped line (e.g. `Note: verification passed`) stays in the body and is not added to `fields`.
- [x] Route the recovered YAML region through the existing `parse_frontmatter_mapping` + `status` validation so malformed/ambiguous YAML and missing `status` fail with precise errors.
- [x] Preserve the strict fully-delimited path (now with hardened closing recognition) and keep genuinely frontmatter-less prose returning `MissingFrontmatter`.
- [x] Preserve a `user_feedback` frontmatter field verbatim through recovery (no rewrite/merge/reorder/append).
- [x] Keep `build_retry_nudge` wording correct for opening/closing/status failures and surfacing the precise reason.
- [x] Review `crates/workflow/agent/src/executor.rs` parse call site/log and keep the plain-prose `matches!(Error::MissingFrontmatter)` assertion valid.
- [x] Review `crates/workflow/engine/src/runner.rs` reason plumbing; confirm no logic change needed and precise `Display` strings reach the retry reason.
- [x] Keep the investigator repro test `recovers_frontmatter_with_omitted_closing_delimiter` unchanged and passing via the product fix.
- [x] Add positive test: blank-line-separated colon-shaped body stays body; assert `fields` has no such key.
- [x] Add positive test: block scalar containing a blank line is fully preserved under the blank-line boundary.
- [x] Add positive test: top-level (`- `) list preserved under recovery.
- [x] Add tests: `---text` and `----` are NOT treated as a closing delimiter (recovery/appropriate error, not a mangled early close).
- [x] Add tests: standalone `---` still closes for LF and CRLF and at EOF (strict path).
- [x] Add positive tests: Markdown heading/prose body, prose preamble before opening delimiter, valid closed block, and `user_feedback` preservation.
- [x] Add negative tests: no opening delimiter (prose) → `MissingFrontmatter`; opening + prose-only region → `MissingClosingDelimiter`; opening + missing `status` → `MissingStatus`; opening + malformed/ambiguous YAML → `Yaml`/`FrontmatterNotMapping`.
- [x] Add/extend the error recoverability test for `MissingClosingDelimiter` in `crates/workflow/agent/src/error.rs`.
- [x] Add/extend the prompt nudge test in `crates/workflow/agent/src/prompt.rs` for the reworded instruction and closing-delimiter reason.
- [x] Run the focused frontmatter/error/prompt tests and the repro command; ensure all pass.
- [x] Run `cargo clippy -p cowboy-workflow-agent --all-targets` and `cargo test --workspace`; fix all compiler and Clippy warnings.
- [x] Commit locally only via Cowboy (replacement/amended commit acceptable; include the required `Co-authored-by` trailer); do not push or open a PR.
