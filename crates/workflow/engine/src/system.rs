//! Best-effort, read-only host/environment context exposed to Lua workflows.
//!
//! Mirrors the kind of identity/environment data `chezmoi` surfaces to its
//! templates (os, arch, hostname, username, home dir, ...). Everything here is
//! best-effort: any field that cannot be determined becomes JSON `null` (Lua
//! `nil`), and gathering never fails a step.
//!
//! Only a curated allow-list of fields is exposed. The full process
//! environment is deliberately NOT dumped into `ctx`, because environment
//! variables frequently contain secrets/tokens that would otherwise leak into
//! persisted run records and event logs.

use serde_json::{Value, json};

/// Builds the curated `ctx.system` table.
///
/// Always returns a JSON object. Individual fields may be `null` when they
/// cannot be determined; the function itself is infallible and never panics.
pub(crate) fn system_context() -> Value {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
        "hostname": whoami::fallible::hostname().ok().map(Value::String).unwrap_or(Value::Null),
        "username": whoami::fallible::username().ok().map(Value::String).unwrap_or(Value::Null),
        "home_dir": home_dir().map(Value::String).unwrap_or(Value::Null),
        "cwd": std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
            .map(Value::String)
            .unwrap_or(Value::Null),
    })
}

/// Best-effort home directory resolution without pulling in an extra crate.
///
/// Reads `HOME` on unix-like systems and `USERPROFILE` on Windows.
fn home_dir() -> Option<String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_context_has_expected_keys() {
        let ctx = system_context();
        let object = ctx.as_object().expect("system context is a JSON object");
        for key in [
            "os", "arch", "family", "hostname", "username", "home_dir", "cwd",
        ] {
            assert!(object.contains_key(key), "missing key: {key}");
        }
    }

    #[test]
    fn system_context_os_arch_match_consts() {
        let ctx = system_context();
        assert_eq!(ctx["os"], json!(std::env::consts::OS));
        assert_eq!(ctx["arch"], json!(std::env::consts::ARCH));
        assert_eq!(ctx["family"], json!(std::env::consts::FAMILY));
    }

    #[test]
    fn system_context_is_infallible() {
        let ctx = system_context();
        assert!(
            ctx.is_object(),
            "system context must always be a JSON object, got {ctx:?}"
        );
    }
}
