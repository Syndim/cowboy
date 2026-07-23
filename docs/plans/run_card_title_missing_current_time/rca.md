# RCA: Card title UIs omit the current time (only workflow-event cards are timestamped)

## Bug behavior

The TUI transcript renders several kinds of "cards". Only workflow-event cards
show the current time in their title. Every other production card either shows
a hardcoded `00:00:00` placeholder or shows no time at all.

Reported example (plain run submission). The title's leading segment is a
frozen `HH:MM:SS` placeholder, not the clock:

```
00:00:00 · ● Run · submitted run
```

The reported title is exactly `00:00:00 · ● Run · submitted run` — an
eight-character `HH:MM:SS` placeholder (`00:00:00`).

Expected: every card UI shows the current wall-clock time in its title prefix,
consistent with workflow-event cards (which already render e.g.
`06:23 · ● Run started · …`).

## Root cause

There is no shared "stamp the current time on a card" policy. Timestamping is
implemented in exactly one place — the workflow-event renderer
(`events.rs::workflow_title_prefix`) — and every other production card
constructor was written without a time prefix. The defect is therefore a
missing cross-cutting timestamp policy, not a single wrong literal.

Production card-construction paths and their current time behavior:

| # | Path | Location | Prefix today | Cards affected |
|---|------|----------|--------------|----------------|
| 0 | `event_card` / `workflow_title_prefix` | `crates/tui/app/src/app/events.rs:409-417` | current time (correct) | all workflow-event cards |
| 1 | `spawn_card_report_task` action helpers | `crates/tui/app/src/app/commands.rs` (`spawn_start_run` and siblings) | literal `"00:00:00"` for Run variants; empty `[]` for Step/Resume/Answer/Resolve | Run, Step, Resume, Answer, Resolve submission cards |
| 2 | `AppState::push_card` | `crates/tui/app/src/app/state.rs:942-952` | `title_prefix: Vec::new()` (empty) | Usage, Notice, Error, Cancelled, Exit, Improve, Resolve-options, Help, Workflows, Prompt |
| 3 | `spawn_runs_list_task` | `crates/tui/app/src/app/state.rs:1093-1109` | `title_prefix: Vec::new()` (empty) | `/runs` loading card |
| 4 | `render_pending_prompt_lines` (direct `Card::new`) | `crates/tui/app/src/app/state.rs:110-139` | no `.title_prefix(..)` call | "Waiting for input" pending-prompt card |

Path 0 is the only one that computes a clock value. Paths 1–4 are the in-scope
defect: the fix must give all four a current-time prefix so "all card UIs" show
the current time.

## Root cause evidence

Logs are not the mechanism here; the defect is deterministic in the render path,
so the walkthrough is grounded in specific source locations (file, function,
line). Each step traces trigger → stored entry → rendered title.

### Path 1 — action-submission card (the reported example)

1. User submits `build health route`.
   `crates/tui/app/src/app/commands.rs::submit_input` →
   `dispatch_submitted_input` takes the non-slash `else` branch (commands.rs:133)
   and calls `spawn_start_run(state, runtime, input)`.

2. `spawn_start_run` (`crates/tui/app/src/app/commands.rs`, `spawn_start_run`)
   builds the card:

   ```rust
   state.spawn_card_report_task(
       "Run",
       ["00:00:00".to_string()],   // <-- hardcoded placeholder prefix
       ["submitted run".to_string()],
       label,
       [body],
       async move { runtime.start_run(request).await... },
   );
   ```

   The second argument is `title_prefix`. It is the literal `"00:00:00"`, never
   computed from `chrono::Local::now()`. Its siblings
   `spawn_start_run_stepwise`, `spawn_start_run_with_workflow`, and
   `spawn_start_run_with_workflow_stepwise` pass the same literal;
   `spawn_step_run`, `spawn_resume_run`, `spawn_answer_task`, and the
   `resolve_run` `Some(status)` branch pass an empty `[]`.

3. `AppState::spawn_card_report_task`
   (`crates/tui/app/src/app/state.rs:1070-1091`) stores the prefix verbatim into
   `TranscriptEntry::Card { title_prefix, .. }`. No clock lookup, no placeholder
   substitution.

4. Rendering: `TranscriptEntry::render_lines_for_width`
   (`crates/tui/app/src/app/state.rs:66-84`) → `render_card_lines` → `app_card`
   (state.rs:169-189) folds each stored prefix into the `Card` via
   `Card::title_prefix`. `Card::title_line` (`crates/tui/app/src/app/card.rs:186-213`)
   joins prefix + leading + suffix with `" · "`, producing:

   ```
   00:00:00 · ● Run · submitted run
   ```

   The placeholder reaches the screen.

### Paths 2–4 — cards with no time prefix at all

5. `AppState::push_card` (`crates/tui/app/src/app/state.rs:942-952`) constructs
   every card it emits with `title_prefix: Vec::new()`:

   ```rust
   self.push_event(TranscriptEntry::Card {
       title: title.to_string(),
       title_prefix: Vec::new(),   // <-- no time
       title_suffix: Vec::new(),
       details: details.into_iter().collect(),
   });
   ```

   In `app_card` the empty prefix vector is folded zero times, so the title line
   is just `"{status} {title}"` (e.g. the idle status icon + `Help`). The first
   `" · "` segment is the status+title text — never a clock. This path serves
   Help, Notice, Error, Cancelled, Exit, Improve, Resolve-options, Usage,
   Workflows, and Prompt cards (see the `push_card` call sites in
   `commands.rs`).

6. `spawn_runs_list_task` (`crates/tui/app/src/app/state.rs:1093-1109`) builds
   the `/runs` loading card with `title_prefix: Vec::new()`, so the same
   no-clock outcome applies to the background-list card.

7. `render_pending_prompt_lines` (`crates/tui/app/src/app/state.rs:110-139`)
   builds the "Waiting for input" card directly with
   `Card::new(status_icon("waiting"), "Waiting for input", CardTone::Warning)`
   and never calls `.title_prefix(..)`. Its title's first `" · "` segment is
   the leading `◷ Waiting for input`, again with no clock.

### Known-good comparison (Path 0)

8. `events.rs::event_card` (`crates/tui/app/src/app/events.rs:409-410`) is the
   only constructor that stamps time:
   `Card::new(status, title, tone).title_prefix(workflow_title_prefix(event))`,
   where `workflow_title_prefix` (events.rs:413-417) formats
   `event.timestamp.with_timezone(&Local)`. This is why workflow-event cards are
   the sole timestamped category and the reported inconsistency exists.

## Reproduction steps

1. Launch the TUI: `cargo run`.
2. Type a plain request such as `build health route` and press `Enter`
   (equivalently `/run build health route`). Observe the card title
   `00:00:00 · ● Run · submitted run` (placeholder time).
3. Type `/help` and press `Enter`. Observe the Help card title begins with the
   status icon and `Help`, with no time.
4. Type `/runs` and press `Enter`. Observe the `Runs` loading card title has no
   time.
5. Drive a workflow to a `WaitingForInput` step. Observe the "Waiting for input"
   card title has no time.
6. Compare with any workflow-event card (e.g. `● Run started · …`), whose title
   begins with the actual current time.

Automated reproduction: the regression test below drives all four in-scope
production card categories plus the timestamped event-card control.

## Regression test

- Test file path: `crates/tui/app/src/app/commands.rs`
- Test name: `all_production_card_uis_show_current_wall_clock_time`
- Command:
  `cargo test -p cowboy --lib all_production_card_uis_show_current_wall_clock_time`
- Expected before fix: FAILS. One command exercises every in-scope production
  card category independently and reports each before asserting. Category 0
  (workflow-event control) passes, proving the assertion recognizes a real
  timestamp; Categories 1–4 all fail because their card titles carry no current
  time. The final assertion lists every failing category.

The test exercises each production card constructor and collects the outcome of
all categories before asserting once (so a single failing category never
short-circuits the others):

- Category 0 (control): a `RunStarted` workflow event → checks the event card is
  timestamped (known-good baseline; must remain passing).
- Category 1: plain `submit_input("build health route")` → action-submission
  card (`spawn_card_report_task`).
- Category 2: `/help` → `push_card` path.
- Category 3: `/runs` → `spawn_runs_list_task` background-list card.
- Category 4: a `WaitingForInput` workflow event → `render_pending_prompt_lines`
  direct `Card::new` path.

For each, it captures `chrono::Local::now()` immediately before and after the
action and checks the rendered card's title prefix (first `" · "` segment) is
neither `00:00:00` nor any non-time value — it must equal the current
wall-clock time formatted as `%H:%M` or `%H:%M:%S` (both accepted so the test
does not over-constrain the fix's chosen format). Each category records a
PASS/FAIL line; the test prints the full report and then asserts the control is
OK and no category failed.

## Current failing result

Structured evidence records for the pre-fix run.

### Tester command records

```json
[
  {
    "id": "tester-cmd-1",
    "source": "investigator",
    "workstation": "local dev workstation",
    "working_dir": "<repo-root>",
    "command": "cargo test -p cowboy --lib all_production_card_uis_show_current_wall_clock_time -- --nocapture",
    "exit_status": "failure (test panicked)"
  }
]
```

### Tester evidence records

```json
[
  {
    "id": "tester-ev-1",
    "source": "investigator",
    "produced_by": "tester-cmd-1",
    "kind": "per-category card-timestamp report + panic",
    "control_category_status": "PASS",
    "failing_categories": ["category1", "category2", "category3", "category4"],
    "output": [
      "card-timestamp category report:",
      "PASS Category 0 workflow-event (control): OK (prefix \"15:28\")",
      "FAIL Category 1 Run action-submission: title prefix is the hardcoded `00:00:00` placeholder; title=00:00:00 · ● Run · submitted run",
      "FAIL Category 2 Help push_card: title prefix \"○ Help\" is not the current wall-clock time (expected one of [\"15:28\", \"15:28:34\"]); title=○ Help",
      "FAIL Category 3 Runs background-list: title prefix \"● Runs\" is not the current wall-clock time (expected one of [\"15:28\", \"15:28:34\"]); title=● Runs · loading runs",
      "FAIL Category 4 Waiting-for-input direct Card::new: title prefix \"◔ Waiting for input\" is not the current wall-clock time (expected one of [\"15:28\", \"15:28:34\"]); title=◔ Waiting for input · ↳ approve · ▶ pending-run",
      "panicked at crates/tui/app/src/app/commands.rs:885:9: the following production card categories do not show the current time: [\"category1\", \"category2\", \"category3\", \"category4\"]",
      "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 331 filtered out"
    ]
  }
]
```

### Implementation / Validator / Reviewer records

No implementation, validation, or review has occurred at the investigation
stage. These stages produce no records yet:

```json
{
  "implementation_command_records": [],
  "implementation_evidence_records": [],
  "validator_command_records": [],
  "validator_evidence_records": [],
  "reviewer_command_records": [],
  "reviewer_evidence_records": [],
  "reviewer_soundness_assessments": []
}
```

Interpretation: the Category 0 event-card control passes, so the assertions
recognize a genuine timestamp; Categories 1–4 all execute and each fails,
independently confirming that every in-scope production card category
(action-submission, `push_card`, background-list, direct `Card::new`
pending-prompt) omits the current time. The wall-clock values (`15:28`,
`15:28:34`) are the run-time clock captured by the test and vary per run.

## Fix constraints

- The fix belongs in `crates/tui/app` only (UI/CLI/dispatch), per `AGENTS.md`;
  do not move card timestamping into runtime crates.
- Give a current-time prefix to all four in-scope production card paths, so
  "all card UIs" show the current time:
  1. Action helpers in `commands.rs` (`spawn_start_run`,
     `spawn_start_run_stepwise`, `spawn_start_run_with_workflow`,
     `spawn_start_run_with_workflow_stepwise`, `spawn_step_run`,
     `spawn_resume_run`, `spawn_answer_task`, and the `resolve_run`
     `Some(status)` branch) — replace the `"00:00:00"` literals and empty
     prefixes.
  2. `AppState::push_card` (`state.rs:942-952`) — stamp the current time.
  3. `spawn_runs_list_task` (`state.rs:1093-1109`) — stamp the current time.
  4. `render_pending_prompt_lines` (`state.rs:110-139`) — add a current-time
     `.title_prefix(..)`.
- Prefer a single shared current-time helper so all card paths (and, ideally,
  the workflow-event path) agree on one format. `events.rs::format_workflow_title_prefix`
  formats `%H:%M` from `.with_timezone(&Local)`; the reported placeholder is
  `%H:%M:%S`-shaped. Pick one format and apply it consistently everywhere.
- `chrono` is already a dependency of the `cowboy` crate
  (`crates/tui/app/Cargo.toml`); no new dependency is required.
- Update existing tests that assert the `00:00:00` placeholder or a
  no-time-prefix title
  (`plain_request_submission_renders_initial_input_as_card`,
  `slash_run_variants_render_initial_input_as_cards`, and any card tests whose
  expected title begins with a status icon rather than a time) to match the new
  time-based behavior.
- Do not reintroduce a placeholder and do not leave any production card without
  a time prefix. Test-only helpers that build cards for unrelated assertions may
  remain untimestamped.
