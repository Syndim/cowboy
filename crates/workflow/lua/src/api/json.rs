use mlua::{LightUserData, Lua, Value};

use crate::Result;
use crate::convert::json_to_lua_preserving_null;

pub(crate) static JSON_NULL: u8 = 0;
pub(crate) const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_BYTES: usize = 64 * 1024;

// serde_json rounds integer lexemes beyond u64 into f64. Check the original
// lexemes before parsing so such integers cannot silently change value.
fn check_numbers(text: &[u8]) -> mlua::Result<()> {
    let mut index = 0;
    while index < text.len() {
        if text[index] == b'"' {
            index += 1;
            while index < text.len() {
                if text[index] == b'\\' {
                    index += 2;
                } else if text[index] == b'"' {
                    index += 1;
                    break;
                } else {
                    index += 1;
                }
            }

            continue;
        }

        if text[index] == b'-' || text[index].is_ascii_digit() {
            let start = index;
            index += 1;
            while index < text.len()
                && (text[index].is_ascii_digit()
                    || matches!(text[index], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                index += 1;
            }

            let lexeme = std::str::from_utf8(&text[start..index]).map_err(mlua::Error::external)?;
            if !lexeme.contains(['.', 'e', 'E']) && lexeme.parse::<i64>().is_err() {
                return Err(mlua::Error::external(
                    "JSON integer exceeds Lua signed integer range",
                ));
            }

            if lexeme.contains(['.', 'e', 'E'])
                && lexeme
                    .parse::<f64>()
                    .is_ok_and(|n| !n.is_finite() || n.abs() > 9_007_199_254_740_992.0)
            {
                return Err(mlua::Error::external(
                    "JSON number exceeds safe floating-point range",
                ));
            }

            continue;
        }

        index += 1;
    }

    Ok(())
}

pub(super) fn install(lua: &Lua, cowboy: &mlua::Table) -> Result<()> {
    let json = lua.create_table()?;
    json.set(
        "null",
        Value::LightUserData(LightUserData((&JSON_NULL as *const u8).cast_mut().cast())),
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, text: mlua::LuaString| {
            if text.as_bytes().len() > MAX_JSON_BYTES {
                return Err(mlua::Error::external("JSON input exceeds 64 KiB"));
            }

            check_numbers(text.as_bytes().as_ref())?;
            let value: serde_json::Value =
                serde_json::from_slice(text.as_bytes().as_ref()).map_err(mlua::Error::external)?;
            json_to_lua_preserving_null(lua, &value, 0).map_err(mlua::Error::external)
        })?,
    )?;
    cowboy.set("json", json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use cowboy_workflow_core::{StepAction, WorkflowSourceSnapshot};

    use crate::runtime::run_step;

    fn source(expression: &str) -> WorkflowSourceSnapshot {
        WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".into(),
            files: BTreeMap::from([(
                "main.lua".into(),
                format!(
                    "local s = step('decode')\ns.run = function(ctx)\n {expression}\nend\nreturn workflow('json', s)"
                ),
            )]),
        }
    }

    #[test]
    fn decodes_command_stdout_into_typed_status_fields() {
        let workflow = source(
            "assert(json == nil and os == nil and io == nil and package == nil and load == nil and setmetatable == nil)\nlocal data = cowboy.json.decode(ctx.prev.fields.stdout)\nassert(data.items[2] == cowboy.json.null)\nreturn action.status { status = 'success', fields = { items = data.items, title = data.title, absent = data.absent } }",
        );
        crate::compile_snapshot(&workflow).unwrap();

        let result = run_step(&workflow, "decode", serde_json::json!({
            "prev": {"fields": {"stdout": "{\n\"items\": [true, null, 42], \"title\": \"café\", \"absent\": null}"}}
        })).unwrap();
        let StepAction::Status(status) = result.action else {
            panic!("expected status action");
        };

        assert_eq!(status.fields["items"], serde_json::json!([true, null, 42]));
        assert_eq!(status.fields["title"], "café");
        assert_eq!(status.fields["absent"], serde_json::Value::Null);
    }

    #[test]
    fn preserves_empty_containers_across_status_and_next_step() {
        let command = source(
            "return action.command { program = 'printf', args = { '{\"object\":{},\"array\":[],\"nested\":{\"a\":{},\"b\":[]},\"items\":[null,{}]}' } }",
        );
        let StepAction::Command(command) = run_step(&command, "decode", serde_json::json!({}))
            .unwrap()
            .action
        else {
            panic!("expected command action");
        };

        let output = std::process::Command::new(&command.program)
            .args(&command.args)
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();

        let workflow = source(
            "local value = cowboy.json.decode(ctx.prev.fields.stdout)\nassert(value.nested.a ~= value.nested.b)\nassert(getmetatable == nil and setmetatable == nil and debug == nil)\nreturn action.status { status = 'success', fields = { object = value.object, array = value.array, nested = value.nested, items = value.items, plain = {} } }",
        );
        let result = run_step(
            &workflow,
            "decode",
            serde_json::json!({"prev": {"fields": {"stdout": stdout}}}),
        )
        .unwrap();
        let StepAction::Status(status) = result.action else {
            panic!("expected status action");
        };

        assert_eq!(status.fields["object"], serde_json::json!({}));
        assert_eq!(status.fields["array"], serde_json::json!([]));
        assert_eq!(
            status.fields["nested"],
            serde_json::json!({"a": {}, "b": []})
        );
        assert_eq!(status.fields["items"], serde_json::json!([null, {}]));
        assert_eq!(status.fields["plain"], serde_json::json!([]));

        let consumer = source(
            "assert(ctx.prev.fields.object ~= nil and ctx.prev.fields.nested.a ~= nil)\nreturn action.status { status = 'success', fields = { saved = ctx.prev.fields } }",
        );
        let next = run_step(
            &consumer,
            "decode",
            serde_json::json!({"prev": {"fields": status.fields}}),
        )
        .unwrap();
        let StepAction::Status(next) = next.action else {
            panic!("expected status action");
        };

        assert_eq!(
            next.fields["saved"]["nested"],
            serde_json::json!({"a": {}, "b": []})
        );
    }

    #[test]
    fn rejects_integer_outside_lua_range_without_rounding() {
        let workflow = source(
            "return action.status { status = 'success', fields = { value = cowboy.json.decode(ctx.prev.fields.stdout) } }",
        );
        for text in ["9223372036854775807", "9223372036854775808"] {
            let result = run_step(
                &workflow,
                "decode",
                serde_json::json!({"prev": {"fields": {"stdout": text}}}),
            );
            if text.ends_with('7') {
                let StepAction::Status(status) = result.unwrap().action else {
                    panic!("expected status action");
                };

                assert_eq!(
                    status.fields["value"],
                    serde_json::json!(9223372036854775807_i64)
                );
            } else {
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn rejects_rounded_integer_lexemes_at_any_depth() {
        let workflow = source(
            "return action.status { status = 'success', fields = { value = cowboy.json.decode(ctx.prev.fields.stdout) } }",
        );
        for text in [
            "18446744073709551616",
            "-9223372036854775809",
            "{\"nested\":[18446744073709551616]}",
            "[{\"n\":-9223372036854775809}]",
            "1e100",
        ] {
            assert!(
                run_step(
                    &workflow,
                    "decode",
                    serde_json::json!({"prev": {"fields": {"stdout": text}}})
                )
                .is_err(),
                "accepted {text}"
            );
        }
    }

    #[test]
    fn rejects_invalid_trailing_oversized_and_deep_json() {
        let workflow = source(
            "return action.status { status = 'success', fields = { value = cowboy.json.decode(ctx.prev.fields.stdout) } }",
        );
        for text in [
            "{",
            "{} true",
            &"x".repeat(64 * 1024 + 1),
            &format!("{}0{}", "[".repeat(65), "]".repeat(65)),
        ] {
            assert!(
                run_step(
                    &workflow,
                    "decode",
                    serde_json::json!({"prev": {"fields": {"stdout": text}}})
                )
                .is_err(),
                "accepted invalid JSON input"
            );
        }
    }
}
