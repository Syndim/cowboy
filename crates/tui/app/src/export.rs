use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use cowboy_workflow_engine::WorkflowRuntime;

use crate::app::card::{SemanticCard, SemanticCardSection};
use crate::app::events::semantic_workflow_event_card;
use crate::app::state::{TranscriptEntry, WorkflowEventProjector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportResult {
    pub run_id: String,
    pub path: PathBuf,
    pub card_count: usize,
}

pub async fn export_run(runtime: &WorkflowRuntime, run_id: &str) -> Result<ExportResult> {
    let run = runtime
        .load_run(run_id)
        .await
        .with_context(|| format!("failed to load run {run_id}"))?;
    let events = runtime
        .load_events(&run.id)
        .with_context(|| format!("failed to load events for run {}", run.id))?;

    let cards = projected_cards(&run.id, run.created_at, &run.original_request, events);

    let html = render_html(&run.id, &cards);
    let filename = export_filename(&run.id);
    let path = runtime.cwd().join(&filename);
    write_complete_file(&path, html.as_bytes())?;

    Ok(ExportResult {
        run_id: run.id,
        path,
        card_count: cards.len(),
    })
}

fn projected_cards(
    run_id: &str,
    created_at: DateTime<Utc>,
    original_request: &str,
    events: Vec<cowboy_workflow_engine::WorkflowEvent>,
) -> Vec<SemanticCard> {
    let mut entries = Vec::new();
    let mut projector = WorkflowEventProjector::default();
    for event in events {
        projector.project(&mut entries, event);
    }

    let mut cards = vec![request_card(run_id, created_at, original_request)];
    cards.extend(entries.into_iter().filter_map(|entry| match entry {
        TranscriptEntry::Workflow {
            event,
            agent_descriptor,
        } => Some(semantic_workflow_event_card(
            &event,
            agent_descriptor.as_deref(),
        )),
        TranscriptEntry::Card { .. } => None,
    }));
    cards
}

fn request_card(run_id: &str, created_at: DateTime<Utc>, request: &str) -> SemanticCard {
    let timestamp = created_at.with_timezone(&Local).format("%H:%M").to_string();
    SemanticCard {
        header: format!("{timestamp} · ◌ Request · ▶ {}", short_run_id(run_id)),
        sections: vec![SemanticCardSection {
            label: Some("Request".to_string()),
            text: request.to_string(),
        }],
    }
}

fn short_run_id(run_id: &str) -> &str {
    run_id.get(..8).unwrap_or(run_id)
}

fn export_filename(run_id: &str) -> String {
    let safe = run_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!(
        "cowboy-export-{}.html",
        if safe.is_empty() { "_" } else { &safe }
    )
}

fn write_complete_file(path: &Path, content: &[u8]) -> Result<()> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("export path has no valid filename")?;
    let temp_path = path.with_file_name(format!(".{filename}.tmp"));
    fs::write(&temp_path, content)
        .with_context(|| format!("failed to write temporary export {}", temp_path.display()))?;
    if let Err(err) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(err).with_context(|| format!("failed to replace export {}", path.display()));
    }
    Ok(())
}

fn render_html(run_id: &str, cards: &[SemanticCard]) -> String {
    let mut html = String::new();
    write!(
        html,
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Cowboy run {}</title>
<style>
:root {{ color-scheme: light dark; font-family: ui-monospace, SFMono-Regular, Consolas, monospace; }}
body {{ margin: 0; background: #111827; color: #e5e7eb; }}
main {{ max-width: 1100px; margin: 0 auto; padding: 24px; }}
h1 {{ font: 600 1.35rem system-ui, sans-serif; margin: 0 0 16px; }}
.controls {{ display: flex; flex-wrap: wrap; gap: 8px; position: sticky; top: 0; padding: 12px 0; background: #111827; z-index: 1; }}
input, button {{ font: inherit; border: 1px solid #4b5563; border-radius: 6px; padding: 8px 10px; background: #1f2937; color: inherit; }}
input {{ flex: 1 1 320px; }}
#match-count {{ align-self: center; min-width: 8rem; }}
details {{ border: 1px solid #374151; border-radius: 8px; margin: 10px 0; background: #1f2937; }}
summary {{ cursor: pointer; padding: 12px 14px; font-weight: 600; white-space: pre-wrap; }}
.body {{ border-top: 1px solid #374151; padding: 4px 14px 14px; }}
section {{ margin-top: 12px; }}
h2 {{ margin: 0 0 6px; font-size: .85rem; color: #93c5fd; }}
pre {{ margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; line-height: 1.45; }}
[hidden] {{ display: none !important; }}
</style>
</head>
<body>
<main>
<h1>Cowboy run {}</h1>
<div class="controls">
<input id="search" type="search" placeholder="Search cards" aria-label="Search cards">
<button id="expand-all" type="button">Expand all</button>
<button id="collapse-all" type="button">Collapse all</button>
<span id="match-count" aria-live="polite">{} cards</span>
</div>
<div id="cards">
"#,
        escape_html(run_id),
        escape_html(run_id),
        cards.len()
    )
    .expect("writing to String cannot fail");

    for card in cards {
        write!(
            html,
            "<details class=\"card\"><summary>{}</summary><div class=\"body\">",
            escape_html(&card.header)
        )
        .expect("writing to String cannot fail");
        for section in &card.sections {
            html.push_str("<section>");
            if let Some(label) = &section.label {
                write!(html, "<h2>{}</h2>", escape_html(label))
                    .expect("writing to String cannot fail");
            }
            write!(html, "<pre>{}</pre></section>", escape_html(&section.text))
                .expect("writing to String cannot fail");
        }
        html.push_str("</div></details>\n");
    }

    html.push_str(
        r#"</div>
</main>
<script>
(() => {
  const cards = Array.from(document.querySelectorAll('.card'));
  const search = document.getElementById('search');
  const count = document.getElementById('match-count');
  const update = () => {
    const query = search.value.toLocaleLowerCase();
    let matches = 0;
    cards.forEach(card => {
      const visible = !query || card.textContent.toLocaleLowerCase().includes(query);
      card.hidden = !visible;
      if (visible) matches += 1;
      card.open = Boolean(query && visible);
    });
    count.textContent = query ? `${matches} match${matches === 1 ? '' : 'es'}` : `${cards.length} cards`;
    if (!query) cards.forEach(card => { card.open = false; });
  };
  search.addEventListener('input', update);
  document.getElementById('expand-all').addEventListener('click', () => cards.forEach(card => {
    if (!card.hidden) card.open = true;
  }));
  document.getElementById('collapse-all').addEventListener('click', () => cards.forEach(card => {
    card.open = false;
  }));
})();
</script>
</body>
</html>
"#,
    );
    html
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

    #[test]
    fn export_filename_is_safe_and_deterministic() {
        assert_eq!(
            export_filename("../run:123<script>"),
            "cowboy-export-___run_123_script_.html"
        );
        assert_eq!(
            export_filename("run-123_ok"),
            "cowboy-export-run-123_ok.html"
        );
        assert_eq!(export_filename(""), "cowboy-export-_.html");
    }

    #[test]
    fn html_is_collapsed_searchable_self_contained_and_escaped() {
        let cards = vec![SemanticCard {
            header: "Header <script> & \"quoted\"".to_string(),
            sections: vec![SemanticCardSection {
                label: Some("Body".to_string()),
                text: "line one\n</script> BODY_ONLY_SEARCH_TOKEN".to_string(),
            }],
        }];
        let html = render_html("run<&", &cards);

        assert!(html.contains("<details class=\"card\"><summary>"));
        assert!(!html.contains("<details open"));
        assert!(html.contains("Header &lt;script&gt; &amp; &quot;quoted&quot;"));
        assert!(html.contains("&lt;/script&gt; BODY_ONLY_SEARCH_TOKEN"));
        assert!(!html.contains("Header <script>"));
        assert!(html.contains("id=\"search\""));
        assert!(html.contains("id=\"match-count\""));
        assert!(html.contains("id=\"expand-all\""));
        assert!(html.contains("id=\"collapse-all\""));
        assert!(html.contains("toLocaleLowerCase"));
        assert!(!html.contains("src=\"http"));
        assert!(!html.contains("href=\"http"));
    }

    #[test]
    fn complete_file_replaces_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.html");
        write_complete_file(&path, b"first").unwrap();
        write_complete_file(&path, b"second").unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "second");
        assert!(!dir.path().join(".export.html.tmp").exists());
    }

    #[test]
    fn projected_export_cards_preserve_order_and_coalesce_stream_updates() {
        let created_at = "2026-01-02T03:04:05Z".parse().unwrap();
        let events = vec![
            WorkflowEvent::new(
                "run-123",
                WorkflowEventKind::AgentResponse {
                    step_id: "start".to_string(),
                    content: "first response line\n".to_string(),
                },
            ),
            WorkflowEvent::new(
                "run-123",
                WorkflowEventKind::AgentResponse {
                    step_id: "start".to_string(),
                    content: "second response line".to_string(),
                },
            ),
            WorkflowEvent::new(
                "run-123",
                WorkflowEventKind::AgentToolCall {
                    step_id: "start".to_string(),
                    tool_call_id: "tool-1".to_string(),
                    title: "Inspect fixture".to_string(),
                    tool_kind: "read".to_string(),
                    status: "running".to_string(),
                },
            ),
            WorkflowEvent::new(
                "run-123",
                WorkflowEventKind::AgentToolCallUpdate {
                    step_id: "start".to_string(),
                    tool_call_id: "tool-1".to_string(),
                    title: "Inspect fixture".to_string(),
                    status: "completed".to_string(),
                    content: Some(serde_json::json!({
                        "output": "TOOL_UPDATE_SEARCH_TOKEN"
                    })),
                },
            ),
            WorkflowEvent::new("run-123", WorkflowEventKind::RunCompleted),
        ];

        let cards = projected_cards("run-123", created_at, "export request", events);
        assert_eq!(cards.len(), 4);
        assert!(cards[0].header.contains("Request"));
        assert_eq!(cards[0].sections[0].text, "export request");
        assert!(cards[1].header.contains("Agent response"));
        assert_eq!(
            cards[1].sections[0].text,
            "first response line\nsecond response line"
        );
        assert!(cards[2].header.contains("Inspect fixture"));
        assert!(cards[2].sections.iter().any(|section| {
            section.label.as_deref() == Some("Output")
                && section.text.contains("TOOL_UPDATE_SEARCH_TOKEN")
        }));
        assert!(cards[3].header.contains("Run completed"));
    }
}
