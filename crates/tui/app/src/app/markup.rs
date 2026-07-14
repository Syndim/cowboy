use std::collections::HashSet;
use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, Theme};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use super::styles::{style_border, style_transcript_code_fallback};

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<syntect::highlighting::ThemeSet> =
    LazyLock::new(syntect::highlighting::ThemeSet::load_defaults);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContentFormat {
    LiteralWithCodeHighlighting,
    Markdown,
}

pub(super) fn render_content(
    text: &str,
    base_style: Style,
    format: ContentFormat,
) -> Vec<Line<'static>> {
    match format {
        ContentFormat::LiteralWithCodeHighlighting => {
            render_literal_with_code_highlighting(text, base_style)
        }
        ContentFormat::Markdown => render_markdown_content(text, base_style),
    }
}

fn render_literal_with_code_highlighting(text: &str, base_style: Style) -> Vec<Line<'static>> {
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

fn render_markdown_content(text: &str, base_style: Style) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![Line::from("")];
    }

    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    MarkdownRenderer::new(base_style).render(Parser::new_ext(text, options))
}

struct MarkdownRenderer {
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    base_style: Style,
    inline_modifiers: Vec<Modifier>,
    quote_depth: usize,
    lists: Vec<MarkdownList>,
    links: Vec<MarkdownLink>,
    image: Option<MarkdownImage>,
    code_block: Option<MarkdownCodeBlock>,
}

impl MarkdownRenderer {
    fn new(base_style: Style) -> Self {
        Self {
            lines: Vec::new(),
            spans: Vec::new(),
            base_style,
            inline_modifiers: Vec::new(),
            quote_depth: 0,
            lists: Vec::new(),
            links: Vec::new(),
            image: None,
            code_block: None,
        }
    }

    fn render<'a>(mut self, events: impl IntoIterator<Item = Event<'a>>) -> Vec<Line<'static>> {
        for event in events {
            self.handle_event(event);
        }

        self.finish_line();
        if self.lines.is_empty() {
            self.lines.push(Line::from(""));
        }

        self.lines
    }

    fn handle_event(&mut self, event: Event<'_>) {
        if let Event::Text(text) = &event
            && let Some(code_block) = self.code_block.as_mut()
        {
            code_block.source.push_str(text);
            return;
        }

        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.append_text(&text, self.current_style()),
            Event::Code(code) => self.append_text(&code, style_transcript_code_fallback()),
            Event::Html(html) | Event::InlineHtml(html) => {
                self.append_text(&html, style_transcript_code_fallback());
            }
            Event::SoftBreak => {
                let style = self.current_style();
                self.append_text(" ", style);
            }
            Event::HardBreak => {
                if let Some(image) = self.image.as_mut() {
                    image.alt.push(' ');
                } else {
                    self.finish_line();
                }
            }
            Event::Rule => {
                self.finish_line();
                self.append_generated("────────────────────────", self.base_style);
                self.finish_line();
            }
            Event::TaskListMarker(checked) => {
                let marker = if checked { "[x] " } else { "[ ] " };
                self.append_generated(marker, self.base_style);
            }
            Event::InlineMath(payload)
            | Event::DisplayMath(payload)
            | Event::FootnoteReference(payload) => {
                self.append_text(&payload, style_transcript_code_fallback());
            }
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { .. } => {
                self.finish_line();
                self.inline_modifiers.push(Modifier::BOLD);
            }
            Tag::BlockQuote(_) => {
                self.finish_line();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.finish_line();
                self.code_block = Some(MarkdownCodeBlock::new(kind));
            }
            Tag::HtmlBlock => self.finish_line(),
            Tag::List(first) => {
                if !self.spans.is_empty() {
                    self.finish_line();
                }

                self.lists.push(MarkdownList { next: first });
            }
            Tag::Item => {
                self.finish_line();
                let prefix = self
                    .lists
                    .last_mut()
                    .map(MarkdownList::take_prefix)
                    .unwrap_or_else(|| "- ".to_string());
                self.append_generated(&prefix, self.base_style);
            }
            Tag::Emphasis => self.inline_modifiers.push(Modifier::ITALIC),
            Tag::Strong => self.inline_modifiers.push(Modifier::BOLD),
            Tag::Strikethrough => self.inline_modifiers.push(Modifier::CROSSED_OUT),
            Tag::Link { dest_url, .. } => self.links.push(MarkdownLink {
                destination: dest_url.into_string(),
                label: String::new(),
            }),
            Tag::Image { dest_url, .. } => {
                self.image = Some(MarkdownImage {
                    destination: dest_url.into_string(),
                    alt: String::new(),
                });
            }
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.finish_line(),
            TagEnd::Heading(_) => {
                self.remove_modifier(Modifier::BOLD);
                self.finish_line();
            }
            TagEnd::BlockQuote(_) => {
                self.finish_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => self.finish_code_block(),
            TagEnd::HtmlBlock => self.finish_line(),
            TagEnd::List(_) => {
                self.finish_line();
                self.lists.pop();
            }
            TagEnd::Item => self.finish_line(),
            TagEnd::Emphasis => self.remove_modifier(Modifier::ITALIC),
            TagEnd::Strong => self.remove_modifier(Modifier::BOLD),
            TagEnd::Strikethrough => self.remove_modifier(Modifier::CROSSED_OUT),
            TagEnd::Link => self.finish_link(),
            TagEnd::Image => self.finish_image(),
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn current_style(&self) -> Style {
        self.inline_modifiers
            .iter()
            .fold(self.base_style, |style, modifier| {
                style.add_modifier(*modifier)
            })
    }

    fn remove_modifier(&mut self, modifier: Modifier) {
        if let Some(index) = self
            .inline_modifiers
            .iter()
            .rposition(|candidate| *candidate == modifier)
        {
            self.inline_modifiers.remove(index);
        }
    }

    fn append_text(&mut self, text: &str, style: Style) {
        if let Some(image) = self.image.as_mut() {
            image.alt.push_str(text);
            return;
        }

        for (index, fragment) in text.split('\n').enumerate() {
            if index > 0 {
                self.finish_line();
            }

            if !fragment.is_empty() {
                if let Some(link) = self.links.last_mut() {
                    link.label.push_str(fragment);
                }

                self.ensure_prefix();
                self.spans.push(Span::styled(fragment.to_string(), style));
            }
        }
    }

    fn append_generated(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }

        self.ensure_prefix();
        self.spans.push(Span::styled(text.to_string(), style));
    }

    fn ensure_prefix(&mut self) {
        if self.spans.is_empty() && self.quote_depth > 0 {
            self.spans
                .push(Span::styled("│ ".repeat(self.quote_depth), self.base_style));
        }
    }

    fn finish_line(&mut self) {
        if !self.spans.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.spans)));
        }
    }

    fn finish_code_block(&mut self) {
        let Some(mut code_block) = self.code_block.take() else {
            return;
        };
        let source = std::mem::take(&mut code_block.source);
        if source.is_empty() {
            self.append_code_line(code_block.highlighter.highlight(""));
            return;
        }

        for raw_line in source.split_terminator('\n') {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let highlighted = code_block.highlighter.highlight(line);
            self.append_code_line(highlighted);
        }
    }

    fn append_code_line(&mut self, line: Line<'static>) {
        self.ensure_prefix();
        self.spans.extend(line.spans);
        self.lines.push(Line::from(std::mem::take(&mut self.spans)));
    }

    fn finish_link(&mut self) {
        let Some(link) = self.links.pop() else {
            return;
        };
        if link.destination != link.label {
            self.append_generated(&format!(" ({})", link.destination), self.base_style);
        }
    }

    fn finish_image(&mut self) {
        let Some(image) = self.image.take() else {
            return;
        };
        let label = if image.alt.is_empty() {
            "[image]".to_string()
        } else {
            format!("[image: {}]", image.alt)
        };
        let rendered = format!("{label} ({})", image.destination);
        if let Some(link) = self.links.last_mut() {
            link.label.push_str(&rendered);
        }

        self.append_generated(&rendered, self.base_style);
    }
}

struct MarkdownList {
    next: Option<u64>,
}

impl MarkdownList {
    fn take_prefix(&mut self) -> String {
        let Some(current) = self.next else {
            return "- ".to_string();
        };
        self.next = Some(current.saturating_add(1));
        format!("{current}. ")
    }
}

struct MarkdownLink {
    destination: String,
    label: String,
}

struct MarkdownImage {
    destination: String,
    alt: String,
}

struct MarkdownCodeBlock {
    highlighter: CodeBlock,
    source: String,
}

impl MarkdownCodeBlock {
    fn new(kind: CodeBlockKind<'_>) -> Self {
        let language = match &kind {
            CodeBlockKind::Indented => None,
            CodeBlockKind::Fenced(info) => info.split_whitespace().next(),
        };
        Self {
            highlighter: CodeBlock::new(language),
            source: String::new(),
        }
    }
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
    let spans = ranges
        .into_iter()
        .map(|(style, content)| Span::styled(content.to_string(), style_from_syntect(style)))
        .collect::<Vec<_>>();
    Some(Line::from(spans))
}

fn style_from_syntect(style: SyntectStyle) -> Style {
    let foreground = color_from_syntect(style.foreground);
    Style {
        fg: foreground,
        bg: color_from_syntect(style.background),
        underline_color: foreground,
        add_modifier: modifier_from_syntect(style.font_style),
        sub_modifier: Modifier::empty(),
    }
}

fn color_from_syntect(color: syntect::highlighting::Color) -> Option<Color> {
    if color.a == 0 {
        return None;
    }

    Some(Color::Rgb(color.r, color.g, color.b))
}

fn modifier_from_syntect(font_style: FontStyle) -> Modifier {
    let mut modifier = Modifier::empty();
    if font_style.contains(FontStyle::BOLD) {
        modifier |= Modifier::BOLD;
    }

    if font_style.contains(FontStyle::ITALIC) {
        modifier |= Modifier::ITALIC;
    }

    if font_style.contains(FontStyle::UNDERLINE) {
        modifier |= Modifier::UNDERLINED;
    }

    modifier
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
        "/run"
            | "/step"
            | "/resume"
            | "/answer"
            | "/runs"
            | "/workflows"
            | "/improve"
            | "/resolve"
            | "/cancel"
            | "/help"
            | "/exit"
    ) || trimmed.starts_with("/run ")
        || trimmed.starts_with("/step ")
        || trimmed.starts_with("/resume ")
        || trimmed.starts_with("/answer ")
        || trimmed.starts_with("/improve ")
        || trimmed.starts_with("/resolve ")
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
    fn markdown_composes_nested_inline_styles_with_base_color() {
        let base_style = Style::default().fg(Color::Cyan);
        let lines = render_content(
            "***nested*** and ~~removed~~",
            base_style,
            ContentFormat::Markdown,
        );
        let line = &lines[0];
        let nested = line
            .spans
            .iter()
            .find(|span| span.content == "nested")
            .unwrap();
        let removed = line
            .spans
            .iter()
            .find(|span| span.content == "removed")
            .unwrap();

        assert_eq!(nested.style.fg, Some(Color::Cyan));
        assert!(nested.style.add_modifier.contains(Modifier::BOLD));
        assert!(nested.style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(removed.style.fg, Some(Color::Cyan));
        assert!(removed.style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn markdown_formats_block_boundaries_prefixes_and_breaks() {
        let lines = render_content(
            "# Heading\n\n3. third\n4. fourth\n\n- plain\n- [x] done\n- [ ] todo\n\n> quoted\n> continued\n\nfirst\nsoft  \nhard\n\n---",
            style_transcript_normal(),
            ContentFormat::Markdown,
        );
        let text = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(
            text,
            vec![
                "Heading",
                "3. third",
                "4. fourth",
                "- plain",
                "- [x] done",
                "- [ ] todo",
                "│ quoted continued",
                "first soft",
                "hard",
                "────────────────────────",
            ]
        );
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn markdown_preserves_links_images_html_and_payload_fallbacks() {
        let base_style = style_transcript_normal();
        let lines = render_content(
            "[Cowboy](https://example.test) [https://same.test](https://same.test) ![diagram](image.png) ![](empty.png)\n\nbefore <kbd>raw</kbd> after",
            base_style,
            ContentFormat::Markdown,
        );

        assert_eq!(
            lines[0].to_string(),
            "Cowboy (https://example.test) https://same.test [image: diagram] (image.png) [image] (empty.png)"
        );
        assert_eq!(lines[1].to_string(), "before <kbd>raw</kbd> after");
        assert!(lines[1].spans.iter().any(|span| {
            span.content == "<kbd>" && span.style == style_transcript_code_fallback()
        }));
        assert!(lines[1].spans.iter().any(|span| {
            span.content == "</kbd>" && span.style == style_transcript_code_fallback()
        }));

        let fallback = MarkdownRenderer::new(base_style)
            .render([Event::FootnoteReference("unhandled payload".into())]);
        assert_eq!(fallback[0].to_string(), "unhandled payload");
        assert_eq!(fallback[0].spans[0].style, style_transcript_code_fallback());
    }

    #[test]
    fn markdown_reuses_code_highlighting_without_delimiters() {
        let inline = render_content(
            "Use `cargo test` now",
            style_transcript_prompt(),
            ContentFormat::Markdown,
        );
        assert_eq!(inline[0].to_string(), "Use cargo test now");
        assert!(inline[0].spans.iter().any(|span| {
            span.content == "cargo test" && span.style == style_transcript_code_fallback()
        }));

        let rust = render_content(
            "```rust\nfn main() { println!(\"hi\"); }\n```",
            style_transcript_normal(),
            ContentFormat::Markdown,
        );
        assert_eq!(rust.len(), 1);
        assert_eq!(rust[0].to_string(), "fn main() { println!(\"hi\"); }");
        assert!(
            foregrounds(&rust[0])
                .into_iter()
                .collect::<HashSet<_>>()
                .len()
                >= 2
        );

        let shell = render_content(
            "```terminal\ncargo test -p cowboy\n```",
            style_transcript_normal(),
            ContentFormat::Markdown,
        );
        assert_eq!(shell[0].to_string(), "cargo test -p cowboy");
        assert_ne!(
            shell[0].spans[0].style.fg,
            style_transcript_code_fallback().fg
        );

        let unknown = render_content(
            "```madeup\nplain text\n```",
            style_transcript_normal(),
            ContentFormat::Markdown,
        );
        assert_eq!(unknown[0].to_string(), "plain text");
        assert_eq!(unknown[0].spans[0].style, style_transcript_code_fallback());

        let indented = render_content(
            "    indented code",
            style_transcript_normal(),
            ContentFormat::Markdown,
        );
        assert_eq!(indented[0].to_string(), "indented code");
        assert_eq!(indented[0].spans[0].style, style_transcript_code_fallback());
    }

    #[test]
    fn markdown_preserves_blank_lines_in_highlighted_fenced_code() {
        let lines = render_content(
            "```rust\nlet first = 1;\n\nlet second = 2;\n```",
            style_transcript_normal(),
            ContentFormat::Markdown,
        );
        let text = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(text, vec!["let first = 1;", "", "let second = 2;"]);
    }

    #[test]
    fn highlights_fenced_rust_code_with_syntect_styles() {
        let lines = render_content(
            "```rust\nfn main() { println!(\"hi\"); }\n```",
            style_transcript_normal(),
            ContentFormat::LiteralWithCodeHighlighting,
        );
        let code_line = &lines[1];
        let styles = foregrounds(code_line).into_iter().collect::<HashSet<_>>();

        assert!(styles.len() >= 2, "{code_line:?}");
        assert!(code_line.to_string().contains("fn main"));
    }

    #[test]
    fn routes_shell_fences_through_shell_syntax() {
        let lines = render_content(
            "```terminal\ncargo test -p cowboy\n```",
            style_transcript_normal(),
            ContentFormat::LiteralWithCodeHighlighting,
        );
        assert_eq!(lines[1].to_string(), "cargo test -p cowboy");
        assert_ne!(
            lines[1].spans.first().and_then(|span| span.style.fg),
            style_transcript_code_fallback().fg
        );
    }

    #[test]
    fn inline_code_uses_code_fallback_style() {
        let line = render_content(
            "Use `cargo test` now",
            style_transcript_prompt(),
            ContentFormat::LiteralWithCodeHighlighting,
        )
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
        let lines = render_content(
            "```madeup\nplain text\n```",
            style_transcript_normal(),
            ContentFormat::LiteralWithCodeHighlighting,
        );
        assert_eq!(lines[1].to_string(), "plain text");
        assert_eq!(lines[1].spans[0].style, style_transcript_code_fallback());
    }

    #[test]
    fn unterminated_code_fence_highlights_until_end() {
        let lines = render_content(
            "```rust\nlet value = 1;",
            style_transcript_normal(),
            ContentFormat::LiteralWithCodeHighlighting,
        );
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
    fn literal_mode_preserves_markdown_delimiters_and_highlights_commands() {
        let lines = render_content(
            "# **literal** and `inline`\ncargo test -p cowboy",
            style_transcript_normal(),
            ContentFormat::LiteralWithCodeHighlighting,
        );

        assert_eq!(lines[0].to_string(), "# **literal** and `inline`");
        assert!(lines[0].spans.iter().any(|span| {
            span.content == "inline" && span.style == style_transcript_code_fallback()
        }));
        assert_eq!(lines[1].to_string(), "cargo test -p cowboy");
        assert_ne!(
            lines[1].spans.first().and_then(|span| span.style.fg),
            style_transcript_code_fallback().fg
        );
    }

    #[test]
    fn markdown_mode_treats_standalone_commands_as_ordinary_text() {
        let base_style = Style::default().fg(Color::Cyan);
        let lines = render_content("cargo test -p cowboy", base_style, ContentFormat::Markdown);

        assert_eq!(lines[0].to_string(), "cargo test -p cowboy");
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].style, base_style);
    }

    #[test]
    fn command_routing_accepts_standalone_commands_and_rejects_prose() {
        assert!(is_command_line("$ cargo test"));
        assert!(is_command_line("cargo run -- run add a route"));
        assert!(is_command_line("/run add a route"));
        assert!(is_command_line("/run --workflow review do work"));
        assert!(is_command_line("/run --step do work"));
        assert!(!is_command_line("/run-workflow"));
        assert!(!is_command_line("/run-workflow review do work"));
        assert!(is_command_line("/resume"));
        assert!(!is_command_line("please run cargo test"));
        assert!(!is_command_line("please resume run-1"));
        assert!(!is_command_line("please run-workflow review do work"));
        assert!(!is_command_line("run-workflow review do work"));
    }
}
