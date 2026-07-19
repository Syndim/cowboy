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
    match find_closing_delimiter(after_open) {
        Some((yaml_end, body_start)) => {
            let yaml = &after_open[..yaml_end];
            let body = after_open[body_start..].trim();
            Ok((yaml, body))
        }
        None => recover_unclosed_frontmatter(after_open),
    }
}

/// Locate a standalone closing `---` delimiter line within `after_open`.
///
/// Only a line whose content is exactly `---` (a single trailing `\r` allowed)
/// terminated by `\n`, `\r\n`, or end of input closes the frontmatter. `---text`,
/// `----`, and `--- ` are not closing delimiters, so they no longer trigger a
/// mangled early close. Returns the byte offset where the YAML region ends (the
/// start of the delimiter line) and the byte offset where the body begins (just
/// past the delimiter line).
fn find_closing_delimiter(after_open: &str) -> Option<(usize, usize)> {
    let mut offset = 0usize;
    for line in after_open.split_inclusive('\n') {
        if is_closing_delimiter_line(line) {
            return Some((offset, offset + line.len()));
        }
        offset += line.len();
    }
    None
}

/// Whether `line` (one line, its terminating newline optionally included) is a
/// standalone closing `---` delimiter: exactly three dashes, allowing a single
/// trailing `\r` and/or `\n`, but nothing else.
fn is_closing_delimiter_line(line: &str) -> bool {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    line == "---"
}

/// Conservative, deterministic recovery for a frontmatter block that has a valid
/// opening `---` delimiter but omits the closing `---` delimiter.
///
/// `after_open` is the text immediately following the opening delimiter (leading
/// CR/LF already stripped). The YAML region is the contiguous leading run of
/// field lines plus their continuations; the body boundary is chosen
/// deterministically with a block-scalar-aware blank-line strategy:
///
/// * blank line — ends the YAML region and starts the body, *unless* it is
///   inside an active `|`/`>` block scalar (those interior blanks are scalar
///   content and are kept);
/// * indented line — a continuation (block-scalar content, nested mapping, or an
///   indented list item) that stays in the YAML region;
/// * a column-0 `- ` line — a block-sequence item of the currently open array,
///   kept in the YAML region;
/// * a column-0 line that parses as a `key:`/`key: value` field — kept, updating
///   whether an array or a block scalar is now open;
/// * any other column-0 line (e.g. a Markdown `## heading` or prose) — starts the
///   body.
///
/// Because a blank line outside a block scalar ends the region, a
/// blank-line-separated colon-shaped body line (for example `Note: verification
/// passed`) stays in the body and is never absorbed as an invented field.
///
/// Recovery only produces a `(yaml, body)` split; the caller still validates the
/// region through the strict-then-lenient mapping parser and the required
/// `status` field, so malformed/ambiguous YAML and schema failures surface as
/// their precise errors. When no top-level field line is present the region is
/// plain prose and `Error::MissingClosingDelimiter` is returned rather than
/// fabricating a mapping.
fn recover_unclosed_frontmatter(after_open: &str) -> Result<(&str, &str)> {
    let mut saw_field = false;
    let mut in_array = false;
    let mut in_block_scalar = false;
    let mut body_start: Option<usize> = None;
    let mut offset = 0usize;

    for line in after_open.split_inclusive('\n') {
        let content_line = line.trim_end_matches(['\r', '\n']);

        if content_line.trim().is_empty() {
            if in_block_scalar {
                // Blank line inside a `|`/`>` block scalar is scalar content.
                offset += line.len();
                continue;
            }
            // A blank line outside a block scalar ends the YAML region; the rest
            // (including any colon-shaped lines) is body.
            body_start = Some(offset);
            break;
        }

        if content_line.starts_with([' ', '\t']) {
            // Indented continuation: block-scalar content, nested mapping, or an
            // indented list item. Stays in the YAML region.
            offset += line.len();
            continue;
        }

        // Column-0, non-blank line: any active block scalar has ended.
        in_block_scalar = false;
        let trimmed = content_line.trim();

        // A top-level list item (`- ...`) is a block-sequence continuation of the
        // preceding `key:` field when that field opened an array. Agents
        // frequently emit list items at column 0.
        if in_array && (trimmed == "-" || trimmed.starts_with("- ")) {
            offset += line.len();
            continue;
        }

        if let Some((_, value)) = parse_lenient_field(content_line) {
            saw_field = true;
            in_array = value.is_empty();
            in_block_scalar = is_block_scalar_header(value);
            offset += line.len();
            continue;
        }

        body_start = Some(offset);
        break;
    }

    if !saw_field {
        return Err(Error::MissingClosingDelimiter);
    }

    let (yaml, body) = match body_start {
        Some(index) => (&after_open[..index], &after_open[index..]),
        None => (after_open, ""),
    };
    Ok((yaml, body.trim()))
}

/// Whether a lenient field's value opens a YAML block scalar (`|` or `>`), with
/// only optional chomping/indentation indicators (`-`, `+`, digits) or a trailing
/// comment after the indicator. A value that merely starts with `|`/`>` followed
/// by other text is a plain scalar, not a block-scalar header.
fn is_block_scalar_header(value: &str) -> bool {
    let value = value.trim();
    let mut chars = value.chars();
    match chars.next() {
        Some('|') | Some('>') => {}
        _ => return false,
    }
    chars
        .take_while(|ch| !ch.is_whitespace())
        .all(|ch| matches!(ch, '-' | '+' | '0'..='9'))
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
    fn rejects_missing_frontmatter() {
        assert!(matches!(
            parse_frontmatter_output("plain text"),
            Err(Error::MissingFrontmatter)
        ));
    }

    // Regression: representative plan/review/commit replies frequently emit the
    // opening `---` delimiter with complete, valid fields plus a Markdown body,
    // but omit the closing `---` delimiter. `split_frontmatter` currently reports
    // this as `MissingFrontmatter`, retries repeat the same output, and useful
    // completed work cannot serialize. Conservative recovery should accept the
    // structured frontmatter and treat the remaining Markdown as the body.
    #[test]
    fn recovers_frontmatter_with_omitted_closing_delimiter() {
        let raw = "---\n\
status: ready\n\
summary: Strict stdlib PNG CRC/structure validation plan\n\
files:\n\
  - scripts/verify.sh\n\
  - tests/verify_crc_corrupt_frame.sh\n\
\n\
## Plan\n\
\n\
1. Add a strict CRC check to the stdlib PNG fallback parser.\n\
2. Add a focused regression test for CRC-corrupted frames.\n";
        let parsed = parse_frontmatter_output(raw)
            .expect("frontmatter with an omitted closing delimiter should be recovered");
        assert_eq!(parsed.output.status, "ready");
        assert_eq!(
            parsed.output.fields["summary"],
            "Strict stdlib PNG CRC/structure validation plan"
        );
        assert_eq!(parsed.output.fields["files"][0], "scripts/verify.sh");
        assert_eq!(
            parsed.output.fields["files"][1],
            "tests/verify_crc_corrupt_frame.sh"
        );
        assert!(
            parsed.output.body.starts_with("## Plan"),
            "body should retain the Markdown content, got: {:?}",
            parsed.output.body
        );
    }

    // Positive: a long `|` block scalar whose content is indented and contains
    // blank lines must stay inside the recovered YAML region; the body starts at
    // the first top-level Markdown heading even though the closing `---` is
    // omitted (mirrors representative review/plan replies).
    #[test]
    fn recovers_frontmatter_with_long_block_scalar() {
        let raw = "---\nstatus: needs_changes\nsummary: Review of the CRC validation plan\ndetails: |\n  The plan is thorough and repository-grounded.\n\n  It correctly identifies the stdlib fallback gap and\n  proposes a focused regression test.\nnotes:\n  - checked README Tests & CI section\n  - confirmed repro test is unchanged\n\n## Review\n\nThe plan references are accurate.\n";
        let parsed = parse_frontmatter_output(raw)
            .expect("block scalar frontmatter with omitted closing delimiter should recover");
        assert_eq!(parsed.output.status, "needs_changes");
        assert_eq!(
            parsed.output.fields["details"],
            "The plan is thorough and repository-grounded.\n\nIt correctly identifies the stdlib fallback gap and\nproposes a focused regression test.\n"
        );
        assert_eq!(
            parsed.output.fields["notes"][0],
            "checked README Tests & CI section"
        );
        assert_eq!(
            parsed.output.fields["notes"][1],
            "confirmed repro test is unchanged"
        );
        assert!(
            parsed.output.body.starts_with("## Review"),
            "body should start at the heading, got: {:?}",
            parsed.output.body
        );
    }

    // Positive: a Markdown heading plus prose body after valid fields with no
    // closing delimiter recovers, preserving the fields and the Markdown body.
    #[test]
    fn recovers_frontmatter_with_heading_and_prose_body() {
        let raw = "---\n\
status: ready\n\
summary: Implement strict IHDR field validation\n\
files:\n\
  - scripts/verify.sh\n\
\n\
## Investigation\n\
\n\
The parser unpacks `_c` and `_f` but never checks them.\n\
This lets malformed frames through.\n";
        let parsed = parse_frontmatter_output(raw)
            .expect("heading/prose body with omitted closing delimiter should recover");
        assert_eq!(parsed.output.status, "ready");
        assert_eq!(
            parsed.output.fields["summary"],
            "Implement strict IHDR field validation"
        );
        assert_eq!(parsed.output.fields["files"][0], "scripts/verify.sh");
        assert!(parsed.output.body.starts_with("## Investigation"));
        assert!(parsed.output.body.contains("lets malformed frames through"));
    }

    // Positive: prose preamble before the opening delimiter combined with an
    // omitted closing delimiter still recovers (mirrors
    // `parses_frontmatter_after_agent_preamble` for the unclosed case).
    #[test]
    fn recovers_frontmatter_after_preamble_without_closing_delimiter() {
        let raw = "Let me record the plan.---\n\
status: ready\n\
summary: Grounded plan\n\
\n\
## Plan\n\
\n\
Do the work.\n";
        let parsed = parse_frontmatter_output(raw)
            .expect("preamble + omitted closing delimiter should recover");
        assert_eq!(parsed.output.status, "ready");
        assert_eq!(parsed.output.fields["summary"], "Grounded plan");
        assert!(parsed.output.body.starts_with("## Plan"));
    }

    // Positive: a fully-delimited (closed) block still parses through the strict
    // path unchanged; the recovery code must not alter closed-block behavior.
    #[test]
    fn valid_closed_block_still_parses_via_strict_path() {
        let raw = "---\nstatus: success\nsummary: done\n---\n\n## Result\n\nAll good.";
        let parsed = parse_frontmatter_output(raw).unwrap();
        assert_eq!(parsed.output.status, "success");
        assert_eq!(parsed.output.fields["summary"], "done");
        assert_eq!(parsed.output.body, "## Result\n\nAll good.");
    }

    // Positive: a `user_feedback` list is cumulative raw user direction and must
    // be carried through recovery verbatim (same order, same items, nothing
    // rewritten or appended).
    #[test]
    fn recovery_preserves_user_feedback_verbatim() {
        let raw = "---\n\
status: implemented\n\
user_feedback:\n\
  - Fix the closing delimiter handling.\n\
  - Add regression tests derived from the logs.\n\
summary: Recovered work\n\
\n\
## Details\n\
\n\
Done.\n";
        let parsed = parse_frontmatter_output(raw).expect("recovery should preserve user_feedback");
        let feedback = parsed.output.fields["user_feedback"]
            .as_array()
            .expect("user_feedback should be an array");
        assert_eq!(feedback.len(), 2);
        assert_eq!(feedback[0], "Fix the closing delimiter handling.");
        assert_eq!(feedback[1], "Add regression tests derived from the logs.");
    }

    // Negative: an opening delimiter followed by a prose-only region (no
    // top-level field lines) and no closing delimiter is not frontmatter; it must
    // report the precise missing-closing-delimiter error, not be recovered.
    #[test]
    fn rejects_opening_delimiter_with_prose_only_region() {
        let raw = "---\n\
This is just prose after a stray delimiter.\n\
It has no fields at all.\n";
        assert!(matches!(
            parse_frontmatter_output(raw),
            Err(Error::MissingClosingDelimiter)
        ));
    }

    // Negative: valid-looking fields but no `status`, with an omitted closing
    // delimiter, must fail the schema check rather than being silently accepted.
    #[test]
    fn rejects_recovered_frontmatter_without_status() {
        let raw = "---\n\
summary: Missing the status field\n\
files:\n\
  - scripts/verify.sh\n\
\n\
## Plan\n\
\n\
Body.\n";
        assert!(matches!(
            parse_frontmatter_output(raw),
            Err(Error::MissingStatus)
        ));
    }

    // Negative: malformed/ambiguous YAML (bad indentation) with an omitted
    // closing delimiter must not be accepted as frontmatter.
    #[test]
    fn rejects_recovered_frontmatter_with_malformed_yaml() {
        let raw = "---\nstatus: ready\nsummary: valid\n  bad_indent: nope\n";
        assert!(matches!(parse_frontmatter_output(raw), Err(Error::Yaml(_))));
    }

    // Negative: genuinely frontmatter-less prose (no opening delimiter) stays
    // classified as the opening-missing variant.
    #[test]
    fn rejects_prose_without_opening_delimiter() {
        assert!(matches!(
            parse_frontmatter_output("Just some prose.\nNo frontmatter here."),
            Err(Error::MissingFrontmatter)
        ));
    }

    // Review defect: a blank-line-separated, top-level colon-shaped body line
    // (e.g. `Note: verification passed`) must remain body and must never be
    // absorbed as an invented YAML field.
    #[test]
    fn recovery_keeps_blank_separated_colon_body_as_body() {
        let raw = "---\n\
status: ready\n\
summary: done\n\
\n\
Note: verification passed\n\
More body text.\n";
        let parsed = parse_frontmatter_output(raw)
            .expect("blank-separated colon body should recover with the fields intact");
        assert_eq!(parsed.output.status, "ready");
        assert_eq!(parsed.output.fields["summary"], "done");
        assert!(
            parsed.output.fields.get("Note").is_none(),
            "colon-shaped body line must not become a field, got: {:?}",
            parsed.output.fields
        );
        assert!(
            parsed.output.body.starts_with("Note: verification passed"),
            "body should retain the colon-shaped line, got: {:?}",
            parsed.output.body
        );
    }

    // Review defect: a blank line *inside* a `|` block scalar is scalar content
    // and must not be treated as the body boundary; the whole scalar is kept and
    // the body starts at the following top-level heading.
    #[test]
    fn recovery_preserves_blank_line_inside_block_scalar() {
        let raw = "---\nstatus: ready\ndetails: |\n  first paragraph\n\n  second paragraph\n\n## Body\n\ntext\n";
        let parsed = parse_frontmatter_output(raw)
            .expect("block scalar with an internal blank line should recover");
        assert_eq!(parsed.output.status, "ready");
        assert_eq!(
            parsed.output.fields["details"],
            "first paragraph\n\nsecond paragraph\n"
        );
        assert!(parsed.output.body.starts_with("## Body"));
    }

    // Review defect: a column-0 (`- `) block sequence under an open key is kept
    // in the recovered YAML region.
    #[test]
    fn recovery_preserves_top_level_list() {
        let raw = "---\nstatus: ready\nfiles:\n- src/a.rs\n- src/b.rs\n\n## Plan\n\nbody\n";
        let parsed = parse_frontmatter_output(raw)
            .expect("top-level list should be kept in the recovered region");
        assert_eq!(parsed.output.fields["files"][0], "src/a.rs");
        assert_eq!(parsed.output.fields["files"][1], "src/b.rs");
        assert!(parsed.output.body.starts_with("## Plan"));
    }

    // Hardening: only a standalone `---` line closes the frontmatter.
    #[test]
    fn is_closing_delimiter_line_recognizes_standalone_dashes_only() {
        assert!(is_closing_delimiter_line("---\n"));
        assert!(is_closing_delimiter_line("---\r\n"));
        assert!(is_closing_delimiter_line("---"));
        assert!(!is_closing_delimiter_line("---text\n"));
        assert!(!is_closing_delimiter_line("----\n"));
        assert!(!is_closing_delimiter_line("--- \n"));
        assert!(!is_closing_delimiter_line("  ---\n"));
    }

    // Hardening: a standalone `---` closes for LF, CRLF, and at end of input.
    #[test]
    fn standalone_closing_delimiter_closes_for_lf_crlf_and_eof() {
        let lf = parse_frontmatter_output("---\nstatus: ready\nsummary: done\n---\nBody.").unwrap();
        assert_eq!(lf.output.status, "ready");
        assert_eq!(lf.output.fields["summary"], "done");
        assert_eq!(lf.output.body, "Body.");

        let crlf =
            parse_frontmatter_output("---\r\nstatus: ready\r\nsummary: done\r\n---\r\nBody.")
                .unwrap();
        assert_eq!(crlf.output.status, "ready");
        assert_eq!(crlf.output.fields["summary"], "done");
        assert_eq!(crlf.output.body, "Body.");

        let eof = parse_frontmatter_output("---\nstatus: ready\nsummary: done\n---").unwrap();
        assert_eq!(eof.output.status, "ready");
        assert_eq!(eof.output.fields["summary"], "done");
        assert_eq!(eof.output.body, "");
    }

    // Hardening: `---text` is not a closing delimiter, so it stays in the body
    // via recovery rather than mangling an early close.
    #[test]
    fn dashes_with_trailing_text_do_not_close_frontmatter() {
        let raw = "---\n\
status: ready\n\
summary: done\n\
\n\
---text is not a delimiter\n\
rest of body\n";
        let parsed = parse_frontmatter_output(raw)
            .expect("`---text` must not close; recovery keeps it in the body");
        assert_eq!(parsed.output.status, "ready");
        assert_eq!(parsed.output.fields["summary"], "done");
        assert!(
            parsed.output.body.starts_with("---text is not a delimiter"),
            "`---text` should remain in the body, got: {:?}",
            parsed.output.body
        );
    }
}
