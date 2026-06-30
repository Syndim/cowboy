use mlua::{Lua, Table, Value};

use crate::Result;

const SAFE_GLOBALS: &[&str] = &[
    "assert", "error", "ipairs", "next", "pairs", "select", "tonumber", "tostring", "type",
];

const SAFE_STRING_FUNCS: &[&str] = &[
    "byte", "char", "find", "format", "gmatch", "gsub", "len", "lower", "match", "rep", "reverse",
    "sub", "upper",
];

const SAFE_TABLE_FUNCS: &[&str] = &[
    "concat", "insert", "move", "pack", "remove", "sort", "unpack",
];

/// Create a Lua VM with an allowlist global environment.
///
/// `Lua::new()` installs the standard Lua globals first. We immediately copy
/// only pure helpers that workflow definitions need, clear every global, and
/// then reinstall the allowlisted values. This is safer than trying to keep a
/// denylist (`os`, `io`, `package`, etc.) complete as Lua or `mlua` evolves.
/// Cowboy-specific APIs (`role`, `step`, `workflow`, `action`, and scoped
/// `require`) are installed later by `api.rs`.
pub fn create_sandbox() -> Result<Lua> {
    let lua = Lua::new();
    let globals = lua.globals();

    let safe_globals = copy_globals(&globals, SAFE_GLOBALS)?;
    let safe_string = copy_table(&lua, &globals.get::<Table>("string")?, SAFE_STRING_FUNCS)?;
    let safe_table = copy_table(&lua, &globals.get::<Table>("table")?, SAFE_TABLE_FUNCS)?;

    let keys = globals
        .clone()
        .pairs::<Value, Value>()
        .filter_map(|pair| pair.ok().map(|(key, _)| key))
        .collect::<Vec<_>>();
    for key in keys {
        globals.set(key, Value::Nil)?;
    }

    for (name, value) in safe_globals {
        globals.set(name, value)?;
    }
    globals.set("string", safe_string)?;
    globals.set("table", safe_table)?;

    Ok(lua)
}

fn copy_globals(globals: &Table, names: &[&str]) -> Result<Vec<(String, Value)>> {
    names
        .iter()
        .map(|name| Ok(((*name).to_string(), globals.get::<Value>(*name)?)))
        .collect()
}

fn copy_table(lua: &Lua, source: &Table, names: &[&str]) -> Result<Table> {
    let table = lua.create_table()?;
    for name in names {
        table.set(*name, source.get::<Value>(*name)?)?;
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_globals_are_absent() {
        let lua = create_sandbox().unwrap();
        for name in [
            "os",
            "io",
            "debug",
            "package",
            "require",
            "dofile",
            "loadfile",
            "load",
            "collectgarbage",
            "coroutine",
            "math",
        ] {
            let value: Value = lua.globals().get(name).unwrap();
            assert!(matches!(value, Value::Nil), "{name} should be absent");
        }
    }

    #[test]
    fn allowlisted_helpers_remain_available() {
        let lua = create_sandbox().unwrap();
        lua.load(
            r#"
            assert(type("abc") == "string")
            assert(string.sub("abcdef", 2, 3) == "bc")
            local values = {"a", "b"}
            assert(table.concat(values, ",") == "a,b")
            "#,
        )
        .exec()
        .unwrap();
    }
}
