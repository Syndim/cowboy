# Plan

Add explicit mouse support to the TUI so vertical wheel events are routed by the region under the pointer instead of being interpreted as global `Up`/`Down` input-history navigation. Mouse scrolling over the transcript (the main section) will move through transcript visual rows using the existing follow-latest state; mouse scrolling over the composer will preserve previous/next submitted-input navigation; the header and status strip will ignore wheel events.

Enable Crossterm mouse capture for the lifetime of the alternate-screen session and always disable it during normal restoration and best-effort setup rollback. Factor the vertical layout into one shared calculation so drawing and mouse hit-testing use the same transcript and composer rectangles after composer-height changes or terminal resizes.

Render a compact one-column vertical scrollbar on the transcript’s right edge when the transcript exceeds its viewport. Derive the thumb from the same wrapped visual-row window used to render content. Extend the existing bounded-tail scan to report the effective offset and whether older content remains, including content truncated within one long streaming entry, so the scrollbar can indicate position without rendering or counting the entire history on every draw.

# Changes

- Update `crates/tui/app/src/app.rs` to:
  - enable `EnableMouseCapture` when entering terminal mode and pair it with `DisableMouseCapture` on every restoration path;
  - dispatch vertical `Event::Mouse` wheel events while continuing to ignore movement, clicks, horizontal scrolling, and unsupported mouse events;
  - centralize the header, transcript, status, and composer rectangles in a small layout value used by both `draw` and mouse hit-testing;
  - mark the draw scheduler dirty only when a handled wheel event can change visible UI state.
- Update `crates/tui/app/src/app/input.rs` with a position-aware mouse-wheel handler:
  - over the transcript rectangle, `ScrollUp` and `ScrollDown` delegate to `AppState::scroll_events_up()` and `scroll_events_down()`;
  - over the composer rectangle, `ScrollUp` and `ScrollDown` delegate to `history_previous()` and `history_next()` under the same submit-availability rule as keyboard `Up`/`Down`;
  - over the header, status strip, or outside the current layout, wheel events are ignored;
  - mouse handling must not mutate composer text while the pointer is over the transcript or mutate transcript position while the pointer is over the composer.
- Keep scroll amount, saturation, and follow-latest transitions in `crates/tui/app/src/app/state.rs`. Add only the minimal return/change reporting needed for the event loop to decide whether a redraw is necessary; do not duplicate offset arithmetic in the mouse handler.
- Refactor `crates/tui/app/src/app/controls/transcript.rs` so its bounded visual-row selection returns a viewport model containing rendered rows, effective row offset, and older/newer overflow information.
  - Preserve the current tail-bounded rendering path and the large-history redraw invariant; the scrollbar must not require materializing all transcript entries.
  - Detect overflow both across earlier entries and within a single long wrapped/streamed entry.
  - Reserve one rightmost column for a `Scrollbar` only when the area is wide enough and content overflows; recalculate wrapping against the remaining content width so the thumb never overwrites card text.
  - Configure a minimal `VerticalRight` scrollbar with no arrow controls and subdued existing TUI colors. Map bottom to follow-latest, top to the oldest reachable row, and intermediate offsets to an upward-moving thumb. Use one bounded overflow sentinel for unmeasured older content rather than scanning the full history solely for proportional sizing.
  - Keep zero-height and very narrow transcript areas panic-free and preserve the existing empty-state rendering.
- If a dedicated scrollbar style is needed, add it in `crates/tui/app/src/app/styles.rs` by composing existing accent/muted palette constants rather than introducing a second color convention.
- Keep all changes inside the `cowboy` TUI crate. Workflow runtime, persisted run state, command grammar, and composer history storage formats remain unchanged.

# Tests to be added/updated

- Add focused mouse-handler tests in `crates/tui/app/src/app/input.rs` proving:
  - wheel up/down inside the transcript changes only transcript scroll/follow state;
  - wheel up/down inside the composer selects previous/next submitted input without changing transcript offset;
  - composer-region scrolling obeys the existing submit-availability guard;
  - wheel events on header/status coordinates and non-vertical mouse events are no-ops;
  - boundary coordinates use the shared layout rectangles after composer-height changes.
- Add terminal/layout coverage in `crates/tui/app/src/app/tests.rs` for the shared rectangle calculation and for matching mouse-capture enter/restore commands, including best-effort cleanup if setup fails after capture is enabled.
- Add transcript renderer tests in `crates/tui/app/src/app/controls/transcript.rs` proving:
  - overflowing wrapped content reserves a one-column right-edge scrollbar without overwriting text;
  - the thumb is at the bottom while following latest, moves upward after scrolling, and returns to the bottom after scrolling down to offset zero;
  - one long transcript entry reports older overflow and displays a scrollbar even when no earlier entry exists;
  - short content does not draw a misleading scrollbar;
  - zero-height and one-column areas remain safe.
- Extend a draw-level test in `crates/tui/app/src/app/tests.rs` with a long transcript to assert the small scrollbar is visible while the status strip and composer borders remain intact.
- Keep the existing large-history redraw regression test passing so scrollbar metrics cannot reintroduce full-history work during ordinary redraws.

# How to verify

- Run `cargo test -p cowboy app::input::tests` for position-aware mouse routing and history/transcript isolation.
- Run `cargo test -p cowboy app::controls::transcript::tests` for visual-row overflow metrics and scrollbar rendering.
- Run `cargo test -p cowboy app::tests` for terminal-mode, layout, full-frame, and large-history regression coverage.
- Run `cargo clippy -p cowboy --all-targets -- -D warnings` and fix all warnings in the touched TUI code.
- Manually run `cargo run -p cowboy`, create enough transcript content to overflow, and verify:
  - wheel scrolling over the main transcript reveals older/newer transcript rows and moves the scrollbar;
  - wheel scrolling over the composer selects previous/next submitted input without moving the transcript;
  - wheel scrolling over the header/status does nothing;
  - returning to the latest transcript restores follow-latest and puts the thumb at the bottom;
  - resizing to narrow and short terminal dimensions keeps the scrollbar, transcript, status, and composer usable;
  - exiting restores normal terminal mouse behavior.

# TODO

- [x] Add paired Crossterm mouse-capture setup, restoration, and failure rollback in `crates/tui/app/src/app.rs`.
- [x] Extract one shared TUI layout calculation for drawing and mouse hit-testing.
- [x] Dispatch vertical mouse-wheel events from the event loop and avoid redraws for ignored events.
- [x] Add transcript-region mouse routing that delegates to the existing transcript scroll methods.
- [x] Add composer-region mouse routing that preserves guarded previous/next input-history behavior.
- [x] Ignore wheel events over non-scrollable regions and ignore unsupported mouse event kinds.
- [x] Add minimal state change reporting without duplicating transcript offset logic outside `AppState`.
- [x] Refactor transcript row selection to return bounded viewport and overflow metadata.
- [x] Detect overflow within both earlier entries and truncated rows of one long entry.
- [x] Reserve a safe one-column transcript scrollbar area and wrap content to the reduced width.
- [x] Render and style the compact right-side scrollbar for overflowing transcript content.
- [x] Add mouse-routing, layout-boundary, and mouse-capture lifecycle tests.
- [x] Add transcript scrollbar position, overflow, narrow-area, and non-overflow tests.
- [x] Extend draw-level coverage for scrollbar visibility alongside status and composer chrome.
- [x] Keep the existing large-history redraw regression test passing.
- [x] Run the focused TUI tests and Clippy checks.
- [x] Manually verify region-specific scrolling, scrollbar movement, resize behavior, and terminal restoration.

# Manual verification evidence

- Date: 2026-07-16.
- Environment: rebuilt `target/debug/cowboy` in a `60×25` PTY, generated overflowing transcript content with four `/help` submissions, and loaded adjacent plain-text history entries `previous submitted` and `next submitted`.
- Transcript wheel: scrolling up changed the rendered transcript rows; the right-edge track ended in `│` instead of the follow-latest thumb `█`.
- Composer wheel: from the stable plain-text `next submitted` selection, wheel-up selected `previous submitted` and wheel-down returned to `next submitted`; rendered transcript rows and the complete scrollbar column remained byte-for-byte unchanged during both actions, demonstrating unchanged transcript offset and scrollbar position.
- Ignored regions: wheel-up over both the header and status strip produced `0` output bytes and no redraw.
- Scrollbar endpoint: scrolling down to latest restored `█` in the final transcript track cell.
- Resize: at `30×8`, transcript text remained visible and readable; wheel-up changed the three transcript rows and wheel-down changed them again; the status strip remained at row index `4`, the scrollbar remained visible, and the composer bottom border remained intact.
- Exit restoration: process exit code was `0`; output contained mouse-disable `?1000l` and bracketed-paste-disable `?2004l`; canonical and echo terminal flags matched their pre-launch values.
