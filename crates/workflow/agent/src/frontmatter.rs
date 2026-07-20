use cowboy_workflow_core::StepOutput;
use serde_json::{Map, Value};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct FrontmatterOutput {
    pub output: StepOutput,
    pub visible_body: String,
}

pub fn parse_frontmatter_output(raw: &str) -> Result<FrontmatterOutput> {
    let (yaml, body) = split_frontmatter(raw)?;
    let mut value = parse_frontmatter_mapping(yaml)?;
    let object = value.as_object_mut().ok_or(Error::FrontmatterNotMapping)?;
    let status = match take_string_field(object, "status")? {
        Some(status) => status,
        None => take_string_field(object, "$status")?.ok_or(Error::MissingStatus)?,
    };
    let fields = Value::Object(std::mem::take(object));
    Ok(FrontmatterOutput {
        output: StepOutput {
            status,
            fields,
            body: body.to_string(),
            raw: Value::String(raw.to_string()),
        },
        visible_body: body.to_string(),
    })
}

fn take_string_field(object: &mut Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.remove(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(Error::FrontmatterFieldNotString(key.to_string())),
    }
}

fn parse_frontmatter_mapping(yaml: &str) -> Result<Value> {
    match serde_yaml::from_str::<serde_yaml::Value>(yaml) {
        Ok(value) => Ok(serde_json::to_value(value)?),
        Err(err) => parse_lenient_frontmatter(yaml).ok_or(Error::Yaml(err)),
    }
}

fn parse_lenient_frontmatter(yaml: &str) -> Option<Value> {
    let mut object = Map::new();
    let mut current_array_key: Option<String> = None;

    for raw_line in yaml.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            continue;
        }

        if line.starts_with(' ') || line.starts_with('\t') {
            let key = current_array_key.as_ref()?;
            let item = parse_lenient_list_item(line)?;
            let Value::Array(items) = object.get_mut(key)? else {
                return None;
            };
            items.push(Value::String(item));
            continue;
        }

        let (key, value) = parse_lenient_field(line)?;
        if value.is_empty() {
            object.insert(key.clone(), Value::Array(Vec::new()));
            current_array_key = Some(key);
        } else {
            object.insert(key, Value::String(clean_lenient_scalar(value)));
            current_array_key = None;
        }
    }

    Some(Value::Object(object))
}

fn parse_lenient_field(line: &str) -> Option<(String, &str)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '$'))
    {
        return None;
    }
    Some((key.to_string(), value.trim_start()))
}

fn parse_lenient_list_item(line: &str) -> Option<String> {
    let value = line.trim_start().strip_prefix("- ")?;
    Some(clean_lenient_scalar(value))
}

fn clean_lenient_scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let quoted = (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'');
        if quoted {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn split_frontmatter(raw: &str) -> Result<(&str, &str)> {
    let Some(open_start) = find_frontmatter_open(raw) else {
        return Err(Error::MissingFrontmatter);
    };
    let frontmatter = &raw[open_start..];
    let after_open = &frontmatter[3..];
    let after_open = after_open.trim_start_matches(['\r', '\n']);
    let Some(close_start) = after_open.find("\n---") else {
        return Err(Error::MissingFrontmatter);
    };
    let yaml = &after_open[..close_start];
    let after_close_marker = &after_open[close_start + 4..];
    let body = after_close_marker
        .strip_prefix('\r')
        .unwrap_or(after_close_marker)
        .strip_prefix('\n')
        .unwrap_or(after_close_marker)
        .trim();
    Ok((yaml, body))
}

fn find_frontmatter_open(raw: &str) -> Option<usize> {
    let mut search_start = raw.len() - raw.trim_start().len();
    while let Some(relative_index) = raw[search_start..].find("---") {
        let index = search_start + relative_index;
        let after = &raw[index + 3..];
        if after.starts_with('\n') || after.starts_with("\r\n") {
            return Some(index);
        }
        search_start = index + 3;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yaml_frontmatter_and_body() {
        let raw =
            "---\nstatus: success\nsummary: done\nfiles:\n  - src/lib.rs\n---\n\nImplemented.";
        let parsed = parse_frontmatter_output(raw).unwrap();
        assert_eq!(parsed.output.status, "success");
        assert_eq!(parsed.output.fields["summary"], "done");
        assert_eq!(parsed.output.fields["files"][0], "src/lib.rs");
        assert_eq!(parsed.output.body, "Implemented.");
    }

    #[test]
    fn supports_dollar_status() {
        let raw = "---\n$status: ready\n---\nbody";
        let parsed = parse_frontmatter_output(raw).unwrap();
        assert_eq!(parsed.output.status, "ready");
    }

    #[test]
    fn parses_colon_rich_array_items_from_agent_frontmatter() {
        let raw = "---\nstatus: ready\nfiles:\n  - docs/tui-omp-layout-proposal.md\n  - crates/tui/src/app.rs\nrisks:\n  - OMP uses a custom append-only renderer; Cowboy should borrow the layout model without replacing ratatui.\nsummary: Create or refresh a reviewable OMP-inspired Cowboy TUI layout proposal, then implement the approved ratatui layout in `app.rs`.\nverification:\n  - Read OMP TUI docs: `omp://tui.md`, `omp://tui-runtime-internals.md`, `omp://tui-core-renderer.md`, `omp://theme.md`.\n  - Read Cowboy TUI baseline: `crates/tui/src/app.rs`.\n---\n\n## Plan\n";
        let parsed = parse_frontmatter_output(raw).unwrap();
        assert_eq!(parsed.output.status, "ready");
        assert_eq!(parsed.output.fields["files"][1], "crates/tui/src/app.rs");
        assert_eq!(
            parsed.output.fields["verification"][0],
            "Read OMP TUI docs: `omp://tui.md`, `omp://tui-runtime-internals.md`, `omp://tui-core-renderer.md`, `omp://theme.md`."
        );
        assert_eq!(parsed.visible_body, "## Plan");
    }

    #[test]
    fn parses_frontmatter_after_agent_preamble() {
        let raw = "I'm sorry, but I cannot assist with that request.---\nstatus: \"committed\"\ncommit: \"5113b61c3e7f51724a1717026efaf05c78f8969a\"\nsummary: \"Committed structured agent progress UI updates\"\n---\n\nCommitted locally.";
        let parsed = parse_frontmatter_output(raw).unwrap();
        assert_eq!(parsed.output.status, "committed");
        assert_eq!(
            parsed.output.fields["commit"],
            "5113b61c3e7f51724a1717026efaf05c78f8969a"
        );
        assert_eq!(
            parsed.output.fields["summary"],
            "Committed structured agent progress UI updates"
        );
        assert_eq!(parsed.output.body, "Committed locally.");
    }
    #[test]
    fn structured_yaml_equality_ignores_formatting_but_preserves_value_semantics() {
        let original = parse_frontmatter_mapping(
            "records:\n  - subject_id: TODO-01\n    procedure:\n      kind: command\n      steps: [cargo test -p sample]\n    exit_status: 0\n  - subject_id: TODO-02\n    exit_status: 7\n",
        )
        .unwrap();
        let reformatted = parse_frontmatter_mapping(
            "records:\n- exit_status: 0\n  procedure: { steps: [\"cargo test -p sample\"], kind: command }\n  subject_id: TODO-01\n- exit_status: 7\n  subject_id: TODO-02\n",
        )
        .unwrap();
        assert_eq!(original, reformatted);

        let reordered = parse_frontmatter_mapping(
            "records:\n  - subject_id: TODO-02\n    exit_status: 7\n  - subject_id: TODO-01\n    procedure:\n      kind: command\n      steps: [cargo test -p sample]\n    exit_status: 0\n",
        )
        .unwrap();
        assert_ne!(original, reordered);

        let changed_value = parse_frontmatter_mapping(
            "records:\n  - subject_id: TODO-01\n    procedure:\n      kind: command\n      steps: [cargo test -p sample]\n    exit_status: 1\n  - subject_id: TODO-02\n    exit_status: 7\n",
        )
        .unwrap();
        assert_ne!(original, changed_value);

        let changed_type = parse_frontmatter_mapping(
            "records:\n  - subject_id: TODO-01\n    procedure:\n      kind: command\n      steps: [cargo test -p sample]\n    exit_status: \"0\"\n  - subject_id: TODO-02\n    exit_status: 7\n",
        )
        .unwrap();
        assert_ne!(original, changed_type);
    }

    #[test]
    fn rejects_missing_frontmatter() {
        assert!(matches!(
            parse_frontmatter_output("plain text"),
            Err(Error::MissingFrontmatter)
        ));
    }
}
