# Add system context to Lua workflows

## Plan

Provide Lua workflows with read-only "system" context about the execution
environment, mirroring the kind of data `chezmoi` exposes to its templates
(`.chezmoi.os`, `.arch`, `.hostname`, `.username`, `.homeDir`, etc.).

Today the runtime builds the Lua `ctx` table in exactly one place:
`LuaStepActionProvider::step_action` in
`crates/workflow/engine/src/runner.rs` (the `json!({ ... })` that becomes
`ctx`). We will extend that `ctx` with a new `ctx.system` table populated by a
small, dependency-light context gatherer in the engine crate.

Design decisions:

- **Injection point:** add a `system` key to the existing `ctx` object in
  `runner.rs`. This is the single, canonical place `ctx` is assembled, so no
  other call sites change.
- **Gatherer location:** new module `crates/workflow/engine/src/system.rs`
  exposing `fn system_context() -> serde_json::Value`. Engine is the product
  runtime layer and is the correct owner for environment/host wiring (core,
  lua, and store must stay environment-agnostic per the design rules).
- **Cross-platform identity:** use the `whoami` crate (same approach chezmoi
  relies on) for reliable `username`/`hostname` across Linux/macOS/Windows,
  instead of fragile `USER`/`HOSTNAME` env reads. `os`/`arch`/`family` come
  from Rust's built-in `std::env::consts`. `home_dir` and `cwd` come from
  `std::env`/`std::env::current_dir`.
- **Curated fields only (security):** expose a fixed allow-list. Do **not**
  dump the full process environment into `ctx`, because environment variables
  frequently contain secrets/tokens. A curated set avoids leaking sensitive
  data into persisted run records and event logs.
- **Never fail a step:** context gathering is best-effort. Any field that
  cannot be determined becomes Lua `nil` (JSON `null`); the step must still
  run. The gatherer never returns an error.

Proposed `ctx.system` shape:

```lua
ctx.system = {
  os       = "linux",        -- std::env::consts::OS
  arch     = "x86_64",       -- std::env::consts::ARCH
  family   = "unix",         -- std::env::consts::FAMILY
  hostname = "my-host",      -- whoami::fallible::hostname()
  username = "alice",        -- whoami::fallible::username()
  home_dir = "/home/alice",  -- home directory, or nil
  cwd      = "/home/alice/project", -- process working directory, or nil
}
```

Out of scope (documented as possible future extension): exposing arbitrary
`ctx.env[...]` environment variables. If added later it must be an explicit,
workflow-declared allow-list to prevent secret leakage, so it is intentionally
excluded here.

## Changes

- `crates/workflow/engine/Cargo.toml`
  - Add dependency `whoami = "1"` (curl-free, cross-platform user/host lookup).

- `crates/workflow/engine/src/system.rs` (new)
  - Add `pub(crate) fn system_context() -> serde_json::Value` that builds the
    curated `system` table. All lookups are best-effort; unavailable values
    serialize as `null`. Prefer `whoami::fallible::username()` /
    `whoami::fallible::hostname()` and map errors to `null`.
  - Add a helper to resolve the home directory without extra deps (read
    `HOME` on unix, `USERPROFILE` on windows) so we avoid pulling in a
    separate home-dir crate.

- `crates/workflow/engine/src/lib.rs` (or `runner.rs` module tree)
  - Declare the new `system` module (e.g. `mod system;`).

- `crates/workflow/engine/src/runner.rs`
  - In `LuaStepActionProvider::step_action`, add `"system": system_context()`
    to the `ctx` `json!` object. No other logic changes.

- `docs/workflow-authoring.md`
  - Add `ctx.system` rows to the "Runtime context passed to `run(ctx)`" table
    and document the field meanings, the best-effort/`nil` semantics, and the
    security note that the full environment is intentionally not exposed.

## Tests to be added/updated

- `crates/workflow/engine/src/system.rs` unit tests:
  - `system_context_has_expected_keys`: returns a JSON object containing
    `os`, `arch`, `family`, `hostname`, `username`, `home_dir`, `cwd` keys.
  - `system_context_os_arch_match_consts`: `os`/`arch`/`family` equal
    `std::env::consts::{OS,ARCH,FAMILY}`.
  - `system_context_is_infallible`: calling the function does not panic and
    always returns a JSON object (never `Value::Null`), even if identity
    lookups fail (values may be `null` but keys are present).

- `crates/workflow/engine/src/runner.rs` (or lua-level) test:
  - Add a test that runs a Lua step whose `step.run(ctx)` returns
    `action.status { status = "success", body = ctx.system.os }` and assert the
    resulting step record body equals `std::env::consts::OS`, proving
    `ctx.system` is threaded through `LuaStepActionProvider` into `run_step`.
    (Model it on the existing `LuaStepActionProvider` tests near the bottom of
    `runner.rs`.)

## How to verify

- `cargo build -p cowboy-workflow-engine`
- `cargo test -p cowboy-workflow-engine`
- `cargo clippy -p cowboy-workflow-engine --all-targets -- -D warnings`
- Manual smoke test: create a temporary workflow whose first step returns
  `action.status { status = "success", body = ctx.system.username .. "@" .. ctx.system.os }`
  and run it via `cargo run -- run smoke` / inspect the run output to confirm
  the system fields are populated.
- Confirm `docs/workflow-authoring.md` renders the new `ctx.system` rows.

## TODO

- [x] Add `whoami = "1"` dependency to `crates/workflow/engine/Cargo.toml`.
- [x] Create `crates/workflow/engine/src/system.rs` with
      `system_context() -> serde_json::Value` (curated, best-effort, infallible).
- [x] Implement best-effort home-dir resolution (`HOME` / `USERPROFILE`).
- [x] Register the `system` module in the engine crate (`mod system;`).
- [x] Inject `"system": system_context()` into the `ctx` `json!` in
      `LuaStepActionProvider::step_action` in `runner.rs`.
- [x] Add unit tests for `system_context()` (keys present, os/arch/family match
      consts, infallible).
- [x] Add a runner/lua test proving `ctx.system.os` reaches a step record.
- [x] Document `ctx.system` fields and the security note in
      `docs/workflow-authoring.md`.
- [x] Run build, targeted tests, and clippy for `cowboy-workflow-engine`;
      fix all warnings before yielding.
