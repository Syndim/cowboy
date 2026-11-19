use mlua::{Lua, Value};

use crate::Result;

const MAX_LOG_BYTES: usize = 64 * 1024;

pub(super) fn install(lua: &Lua, cowboy: &mlua::Table) -> Result<()> {
    let log = lua.create_table()?;
    for level in ["error", "warn", "info", "debug", "trace"] {
        log.set(
            level,
            lua.create_function(move |_, message: Value| {
                let Value::String(message) = message else {
                    return Err(mlua::Error::external("log message must be a string"));
                };
                if message.as_bytes().len() > MAX_LOG_BYTES {
                    return Err(mlua::Error::external("log message exceeds 64 KiB"));
                }
                let message = message.to_str()?;
                let message: &str = &message;
                match level {
                    "error" => {
                        tracing::error!(target: "cowboy_workflow_lua::workflow", workflow_message = ?message, "Lua workflow log")
                    }
                    "warn" => {
                        tracing::warn!(target: "cowboy_workflow_lua::workflow", workflow_message = ?message, "Lua workflow log")
                    }
                    "info" => {
                        tracing::info!(target: "cowboy_workflow_lua::workflow", workflow_message = ?message, "Lua workflow log")
                    }
                    "debug" => {
                        tracing::debug!(target: "cowboy_workflow_lua::workflow", workflow_message = ?message, "Lua workflow log")
                    }
                    "trace" => {
                        tracing::trace!(target: "cowboy_workflow_lua::workflow", workflow_message = ?message, "Lua workflow log")
                    }
                    _ => unreachable!(),
                }
                Ok(())
            })?,
        )?;
    }
    cowboy.set("log", log)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use cowboy_workflow_core::WorkflowSourceSnapshot;
    use tracing_subscriber::EnvFilter;

    use crate::{compile_snapshot, runtime::run_step};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn snapshot(step_body: &str) -> WorkflowSourceSnapshot {
        WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".into(),
            files: BTreeMap::from([
                (
                    "main.lua".into(),
                    format!(
                        "require('helper.lua')\ncowboy.log.info('definition-marker')\nlocal s = step('test')\ns.run = function(ctx)\n{step_body}\nreturn action.status {{ status = 'success' }}\nend\nreturn workflow('test', s)"
                    ),
                ),
                (
                    "helper.lua".into(),
                    "cowboy.log.info('import-marker')\nreturn {}".into(),
                ),
            ]),
        }
    }

    #[test]
    fn logs_levels_from_definitions_imports_and_steps_with_filtering() {
        let source = snapshot(
            "cowboy.log.error('error-marker')\ncowboy.log.warn('warn-marker')\ncowboy.log.info('info-marker')\ncowboy.log.debug('debug-marker')\ncowboy.log.trace('trace-marker')",
        );
        for (directive, debug, trace) in [
            ("info", false, false),
            ("info,cowboy_workflow_lua::workflow=trace", true, true),
        ] {
            let sink = Sink::default();
            let writer = sink.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::new(directive))
                .with_ansi(false)
                .without_time()
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                compile_snapshot(&source).unwrap();
                run_step(&source, "test", serde_json::json!({})).unwrap();
            });
            let bytes = sink.0.lock().unwrap();
            let log = std::str::from_utf8(&bytes).unwrap();
            assert_eq!(log.matches("definition-marker").count(), 2);
            assert_eq!(log.matches("import-marker").count(), 2);
            for (level, marker) in [
                ("ERROR", "error-marker"),
                ("WARN", "warn-marker"),
                ("INFO", "info-marker"),
            ] {
                assert!(log.lines().any(|line| line.contains(level)
                    && line.contains(marker)
                    && line.contains("cowboy_workflow_lua::workflow")));
            }
            assert_eq!(log.contains("debug-marker"), debug);
            assert_eq!(log.contains("trace-marker"), trace);
        }
    }

    #[test]
    fn escapes_control_characters_in_diagnostic_output() {
        let sink = Sink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            run_step(
                &snapshot("cowboy.log.info('first' .. string.char(10, 27) .. 'second')"),
                "test",
                serde_json::json!({}),
            )
            .unwrap();
        });
        let bytes = sink.0.lock().unwrap();
        let log = std::str::from_utf8(&bytes).unwrap();
        assert!(log.contains(r"first\n\u{1b}second"));
        assert!(!log.contains('\u{1b}'));
    }

    #[test]
    fn validates_message_type_encoding_and_size() {
        for body in [
            "cowboy.log.info()",
            "cowboy.log.info({})",
            "cowboy.log.info(42)",
            "cowboy.log.info(string.char(255))",
            "cowboy.log.info(string.rep('a', 65537))",
        ] {
            assert!(
                run_step(&snapshot(body), "test", serde_json::json!({})).is_err(),
                "accepted {body}"
            );
        }
        assert!(
            run_step(
                &snapshot("cowboy.log.info(string.rep('a', 65536))"),
                "test",
                serde_json::json!({}),
            )
            .is_ok()
        );
    }
}
