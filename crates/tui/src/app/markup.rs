use std::collections::HashSet;
use std::sync::LazyLock;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::{SyntaxReference, SyntaxSet};

use super::styles::{style_border, style_transcript_code_fallback};

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<syntect::highlighting::ThemeSet> =
    LazyLock::new(syntect::highlighting::ThemeSet::load_defaults);

pub(super) fn render_markup(text: &str, base_style: Style) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![Line::from("")];
    }

    let mut rendered = Vec::new();
    let mut code_block: Option<CodeBlock> = None;

    for raw_line in text.lines() {
        if let Some(language) = fence_language(raw_line) {
            if code_block.is_some() {
                code_block = None;
            } else {
                code_block = Some(CodeBlock::new(language));
            }
            rendered.push(Line::from(Span::styled(
                raw_line.to_string(),
                style_border(),
            )));
            continue;
        }

        if let Some(block) = code_block.as_mut() {
            rendered.push(block.highlight(raw_line));
        } else if is_command_line(raw_line) {
            rendered.push(highlight_syntax_line(raw_line, Some("sh")));
        } else {
            rendered.push(render_inline_code(raw_line, base_style));
        }
    }

    rendered
}

fn render_inline_code(line: &str, base_style: Style) -> Line<'static> {
    if !line.contains('`') {
        return Line::from(Span::styled(line.to_string(), base_style));
    }

    let mut spans = Vec::new();
    let mut remaining = line;
    let mut in_code = false;
    while let Some(index) = remaining.find('`') {
        let (before, after_tick) = remaining.split_at(index);
        if !before.is_empty() {
            let style = if in_code {
                style_transcript_code_fallback()
            } else {
                base_style
            };
            spans.push(Span::styled(before.to_string(), style));
        }
        spans.push(Span::styled("`", style_transcript_code_fallback()));
        remaining = &after_tick[1..];
        in_code = !in_code;
    }
    if !remaining.is_empty() {
        let style = if in_code {
            style_transcript_code_fallback()
        } else {
            base_style
        };
        spans.push(Span::styled(remaining.to_string(), style));
    }
    Line::from(spans)
}

struct CodeBlock {
    syntax_token: Option<String>,
    highlighter: Option<HighlightLines<'static>>,
}

impl CodeBlock {
    fn new(language: Option<&str>) -> Self {
        let syntax = language.and_then(resolve_syntax);
        let highlighter = syntax.map(|syntax| HighlightLines::new(syntax, syntax_theme()));
        Self {
            syntax_token: language.map(ToOwned::to_owned),
            highlighter,
        }
    }

    fn highlight(&mut self, line: &str) -> Line<'static> {
        if let Some(highlighter) = self.highlighter.as_mut() {
            highlight_with(highlighter, line).unwrap_or_else(|| fallback_code_line(line))
        } else if self
            .syntax_token
            .as_deref()
            .is_some_and(|token| shell_syntax_token(token).is_some())
        {
            highlight_syntax_line(line, Some("sh"))
        } else {
            fallback_code_line(line)
        }
    }
}

fn highlight_syntax_line(line: &str, token: Option<&str>) -> Line<'static> {
    let Some(syntax) = token.and_then(resolve_syntax) else {
        return fallback_code_line(line);
    };
    let mut highlighter = HighlightLines::new(syntax, syntax_theme());
    highlight_with(&mut highlighter, line).unwrap_or_else(|| fallback_code_line(line))
}

fn highlight_with(highlighter: &mut HighlightLines<'_>, line: &str) -> Option<Line<'static>> {
    let ranges = highlighter.highlight_line(line, syntax_set()).ok()?;
    let mut spans = Vec::new();
    for segment in ranges {
        let span = syntect_tui::into_span(segment).ok()?;
        spans.push(Span::styled(span.content.into_owned(), span.style));
    }
    Some(Line::from(spans))
}

fn fallback_code_line(line: &str) -> Line<'static> {
    Line::from(Span::styled(
        line.to_string(),
        style_transcript_code_fallback(),
    ))
}

fn fence_language(line: &str) -> Option<Option<&str>> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("```")?;
    let language = rest
        .split_whitespace()
        .next()
        .filter(|language| !language.is_empty());
    Some(language)
}

fn resolve_syntax(token: &str) -> Option<&'static SyntaxReference> {
    let token = shell_syntax_token(token).unwrap_or(token).trim();
    if token.is_empty() {
        return None;
    }
    syntax_set()
        .find_syntax_by_token(token)
        .or_else(|| syntax_set().find_syntax_by_extension(token))
}

fn shell_syntax_token(token: &str) -> Option<&'static str> {
    match token.trim().to_ascii_lowercase().as_str() {
        "bash" | "sh" | "shell" | "zsh" | "console" | "terminal" => Some("sh"),
        _ => None,
    }
}

fn syntax_set() -> &'static SyntaxSet {
    &SYNTAX_SET
}

fn syntax_theme() -> &'static Theme {
    THEME_SET
        .themes
        .get("base16-ocean.dark")
        .or_else(|| THEME_SET.themes.values().next())
        .expect("syntect bundled themes are available")
}

fn is_command_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.contains('`') {
        return false;
    }
    if trimmed.starts_with("$ ") {
        return trimmed.len() > 2;
    }
    if matches!(
        trimmed,
        "/run" | "/runs" | "/resume" | "/workflows" | "/help" | "/exit"
    ) || trimmed.starts_with("/run ")
        || trimmed.starts_with("/resume ")
        || trimmed.starts_with("/improve ")
    {
        return true;
    }
    let first = trimmed.split_whitespace().next().unwrap_or_default();
    command_names().contains(first)
}

fn command_names() -> &'static HashSet<&'static str> {
    static COMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
        [
            "bun", "cargo", "cat", "cmake", "cp", "curl", "deno", "docker", "git", "just",
            "kubectl", "less", "make", "mkdir", "mv", "node", "npm", "pip", "pnpm", "python",
            "python3", "rm", "rustc", "rustup", "scp", "ssh", "tail", "touch", "uv", "wget",
            "yarn",
        ]
        .into_iter()
        .collect()
    });
    &COMMANDS
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::app::styles::{style_transcript_normal, style_transcript_prompt};

    fn foregrounds(line: &Line<'_>) -> Vec<Option<Color>> {
        line.spans.iter().map(|span| span.style.fg).collect()
    }

    #[test]
    fn highlights_fenced_rust_code_with_syntect_styles() {
        let lines = render_markup(
            "```rust\nfn main() { println!(\"hi\"); }\n```",
            style_transcript_normal(),
        );
        let code_line = &lines[1];
        let styles = foregrounds(code_line).into_iter().collect::<HashSet<_>>();

        assert!(styles.len() >= 2, "{code_line:?}");
        assert!(code_line.to_string().contains("fn main"));
    }

    #[test]
    fn routes_shell_fences_through_shell_syntax() {
        let lines = render_markup(
            "```terminal\ncargo test -p cowboy\n```",
            style_transcript_normal(),
        );
        assert_eq!(lines[1].to_string(), "cargo test -p cowboy");
        assert_ne!(
            lines[1].spans.first().and_then(|span| span.style.fg),
            style_transcript_code_fallback().fg
        );
    }

    #[test]
    fn inline_code_uses_code_fallback_style() {
        let line = render_markup("Use `cargo test` now", style_transcript_prompt())
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(line.to_string(), "Use `cargo test` now");
        assert!(
            line.spans.iter().any(|span| span.content == "cargo test"
                && span.style == style_transcript_code_fallback())
        );
    }

    #[test]
    fn unknown_language_fence_uses_code_fallback_style() {
        let lines = render_markup("```madeup\nplain text\n```", style_transcript_normal());
        assert_eq!(lines[1].to_string(), "plain text");
        assert_eq!(lines[1].spans[0].style, style_transcript_code_fallback());
    }

    #[test]
    fn unterminated_code_fence_highlights_until_end() {
        let lines = render_markup("```rust\nlet value = 1;", style_transcript_normal());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].to_string(), "let value = 1;");
        assert!(
            foregrounds(&lines[1])
                .into_iter()
                .collect::<HashSet<_>>()
                .len()
                >= 2
        );
    }

    #[test]
    fn command_routing_accepts_standalone_commands_and_rejects_prose() {
        assert!(is_command_line("$ cargo test"));
        assert!(is_command_line("cargo run -- run add a route"));
        assert!(is_command_line("/run add a route"));
        assert!(is_command_line("/resume"));
        assert!(is_command_line("/resume run-1"));
        assert!(!is_command_line("please run cargo test"));
        assert!(!is_command_line("please resume run-1"));
    }
}
