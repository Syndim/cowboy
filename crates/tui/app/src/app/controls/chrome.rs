use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::state::AppState;

const SEPARATOR: &str = " · ";

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetadataPartKind {
    Fixed,
    Step,
    Run,
    Workflow,
    Tasks,
}

struct MetadataPart {
    text: String,
    kind: MetadataPartKind,
}

impl MetadataPart {
    fn fixed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: MetadataPartKind::Fixed,
        }
    }

    fn optional(text: impl Into<String>, kind: MetadataPartKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}

pub(super) fn status_icon(status: &str) -> &'static str {
    match status {
        "idle" => "○",
        "running" => "●",
        "waiting" => "◔",
        "retrying" => "↻",
        "completed" => "✓",
        "failed" => "✗",
        "cancelled" | "canceled" => "■",
        _ => "?",
    }
}

fn short_run_id(run_id: &str) -> &str {
    run_id
        .strip_prefix("run-")
        .and_then(|rest| rest.split('-').next())
        .filter(|segment| segment.len() >= 8)
        .unwrap_or(run_id)
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn joined_width(parts: &[MetadataPart]) -> usize {
    parts
        .iter()
        .map(|part| display_width(&part.text))
        .sum::<usize>()
        + display_width(SEPARATOR).saturating_mul(parts.len().saturating_sub(1))
}

fn remove_lowest_priority_part(parts: &mut Vec<MetadataPart>) -> bool {
    for kind in [
        MetadataPartKind::Tasks,
        MetadataPartKind::Workflow,
        MetadataPartKind::Run,
        MetadataPartKind::Step,
    ] {
        if let Some(index) = parts.iter().position(|part| part.kind == kind) {
            parts.remove(index);
            return true;
        }
    }

    false
}

fn join_parts(parts: &[MetadataPart]) -> String {
    parts
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join(SEPARATOR)
}

pub(super) fn truncate_to_display_width(text: impl AsRef<str>, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let text = text.as_ref();
    if display_width(text) <= width {
        return text.to_string();
    }

    let target = width.saturating_sub(display_width("…"));
    let mut truncated = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > target {
            break;
        }

        truncated.push(ch);
        used += ch_width;
    }

    truncated.push('…');
    truncated
}

pub(super) fn status_metadata_text(state: &AppState, width: usize) -> String {
    let display_state = state.display_state();
    let mut parts = vec![MetadataPart::fixed(status_icon(&display_state))];

    if let Some(step) = state.current_step() {
        parts.push(MetadataPart::optional(
            format!("↳ {step}"),
            MetadataPartKind::Step,
        ));
    }

    if let Some(run_id) = state.active_run_id() {
        parts.push(MetadataPart::optional(
            format!("▶ {}", short_run_id(run_id)),
            MetadataPartKind::Run,
        ));
    }

    if let Some(workflow) = state.workflow_name() {
        parts.push(MetadataPart::optional(
            format!("⎇ {workflow}"),
            MetadataPartKind::Workflow,
        ));
    }

    if state.background_task_count() > 0 {
        parts.push(MetadataPart::optional(
            format!("◷ {}", state.background_task_count()),
            MetadataPartKind::Tasks,
        ));
    }

    while width > 0 && joined_width(&parts) > width && remove_lowest_priority_part(&mut parts) {}

    truncate_to_display_width(join_parts(&parts), width)
}
