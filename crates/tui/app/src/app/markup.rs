use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, Theme};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use super::styles::style_transcript_code_fallback;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<syntect::highlighting::ThemeSet> =
    LazyLock::new(syntect::highlighting::ThemeSet::load_defaults);

pub(super) fn render_content(text: &str, base_style: Style) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![Line::from("")];
    }

    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
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
    table_cell_index: Option<usize>,
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
            table_cell_index: None,
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
                if let Some(image) = self.image.as_mut() {
                    image.alt.push(' ');
                } else {
                    if let Some(link) = self.links.last_mut() {
                        link.label.push(' ');
                    }

                    self.finish_line();
                }
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
            Tag::Table(_) => self.finish_line(),
            Tag::TableHead => {
                self.finish_line();
                self.table_cell_index = Some(0);
                self.inline_modifiers.push(Modifier::BOLD);
            }
            Tag::TableRow => {
                self.finish_line();
                self.table_cell_index = Some(0);
            }
            Tag::TableCell => self.start_table_cell(),
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
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
            TagEnd::Table => {
                self.finish_line();
                self.table_cell_index = None;
            }
            TagEnd::TableHead => {
                self.remove_modifier(Modifier::BOLD);
                self.finish_line();
                self.table_cell_index = None;
            }
            TagEnd::TableRow => {
                self.finish_line();
                self.table_cell_index = None;
            }
            TagEnd::TableCell => {}
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn start_table_cell(&mut self) {
        let cell_index = self.table_cell_index.get_or_insert(0);
        if *cell_index > 0 {
            self.append_generated(" │ ", self.base_style);
        }

        *self.table_cell_index.get_or_insert(0) += 1;
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

#[cfg(test)]
mod tests {
    use ratatui::style::Color;
    use std::collections::HashSet;

    use super::*;
    use crate::app::styles::{style_transcript_normal, style_transcript_prompt};

    fn foregrounds(line: &Line<'_>) -> Vec<Option<Color>> {
        line.spans.iter().map(|span| span.style.fg).collect()
    }

    #[test]
    fn markdown_composes_nested_inline_styles_with_base_color() {
        let base_style = Style::default().fg(Color::Cyan);
        let lines = render_content("*italic* **bold** ***nested*** and ~~removed~~", base_style);
        let line = &lines[0];
        let italic = line
            .spans
            .iter()
            .find(|span| span.content == "italic")
            .unwrap();
        let bold = line
            .spans
            .iter()
            .find(|span| span.content == "bold")
            .unwrap();
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

        assert_eq!(italic.style.fg, Some(Color::Cyan));
        assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(bold.style.fg, Some(Color::Cyan));
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
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
                "│ quoted",
                "│ continued",
                "first",
                "soft",
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
        );

        assert_eq!(
            lines[0].to_string(),
            "Cowboy (https://example.test) https://same.test [image: diagram] (image.png) [image] (empty.png)"
        );

        let multiline = render_content("[foo\nbar](foobar) [same](same)", base_style);
        let multiline_text = multiline
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(multiline_text, vec!["foo", "bar (foobar) same"]);
        assert!(!multiline_text.join("\n").contains("(same)"));
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
    fn markdown_preserves_multiline_image_alt_spacing() {
        let lines = render_content(
            "![first line\nsecond line](image.png)",
            style_transcript_normal(),
        );

        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].to_string(),
            "[image: first line second line] (image.png)"
        );
    }

    #[test]
    fn markdown_renders_block_html_math_and_footnote_fallbacks() {
        let fallback_style = style_transcript_code_fallback();
        let html = render_content(
            "<section>\nblock payload\n</section>",
            style_transcript_normal(),
        );
        let payloads = MarkdownRenderer::new(style_transcript_normal()).render([
            Event::InlineMath("x + y".into()),
            Event::SoftBreak,
            Event::DisplayMath("z = 1".into()),
            Event::SoftBreak,
            Event::FootnoteReference("note".into()),
        ]);

        assert_eq!(
            html.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec!["<section>", "block payload", "</section>"]
        );
        assert!(
            html.iter()
                .flat_map(|line| line.spans.iter())
                .all(|span| span.style == fallback_style)
        );
        assert_eq!(
            payloads.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec!["x + y", "z = 1", "note"]
        );
        assert!(
            payloads
                .iter()
                .flat_map(|line| line.spans.iter())
                .all(|span| span.style == fallback_style)
        );
    }

    #[test]
    fn markdown_renders_gfm_tables_as_distinct_styled_rows() {
        let base_style = Style::default().fg(Color::Cyan);
        let lines = render_content(
            "| Item | State | Command |\n| --- | --- | --- |\n| *first* | **done** | `cargo test` |\n| [Cowboy](https://example.test) | pending | next |\n\nAfter table",
            base_style,
        );
        let text = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(
            text,
            vec![
                "Item │ State │ Command",
                "first │ done │ cargo test",
                "Cowboy (https://example.test) │ pending │ next",
                "After table",
            ]
        );
        assert!(
            !text
                .iter()
                .any(|line| line.contains("---") || line.contains('|'))
        );
        assert!(lines[0].spans.iter().any(|span| {
            span.content == "Item"
                && span.style.fg == Some(Color::Cyan)
                && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(lines[1].spans.iter().any(|span| {
            span.content == "first" && span.style.add_modifier.contains(Modifier::ITALIC)
        }));
        assert!(lines[1].spans.iter().any(|span| {
            span.content == "done" && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(lines[1].spans.iter().any(|span| {
            span.content == "cargo test" && span.style == style_transcript_code_fallback()
        }));
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|span| span.content == "Cowboy" && span.style == base_style)
        );
        assert!(
            lines[2].spans.iter().any(|span| {
                span.content == " (https://example.test)" && span.style == base_style
            })
        );
    }

    #[test]
    fn markdown_reuses_code_highlighting_without_delimiters() {
        let inline = render_content("Use `cargo test` now", style_transcript_prompt());
        assert_eq!(inline[0].to_string(), "Use cargo test now");
        assert!(inline[0].spans.iter().any(|span| {
            span.content == "cargo test" && span.style == style_transcript_code_fallback()
        }));

        let rust = render_content(
            "```rust\nfn main() { println!(\"hi\"); }\n```",
            style_transcript_normal(),
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
        );
        assert_eq!(shell[0].to_string(), "cargo test -p cowboy");
        assert_ne!(
            shell[0].spans[0].style.fg,
            style_transcript_code_fallback().fg
        );

        let shell_alias = render_content(
            "```shell\nprintf '%s\\n' done\n```",
            style_transcript_normal(),
        );
        assert_eq!(shell_alias[0].to_string(), "printf '%s\\n' done");
        assert_ne!(
            shell_alias[0].spans[0].style.fg,
            style_transcript_code_fallback().fg
        );

        let unknown = render_content("```madeup\nplain text\n```", style_transcript_normal());
        assert_eq!(unknown[0].to_string(), "plain text");
        assert_eq!(unknown[0].spans[0].style, style_transcript_code_fallback());

        let indented = render_content("    indented code", style_transcript_normal());
        assert_eq!(indented[0].to_string(), "indented code");
        assert_eq!(indented[0].spans[0].style, style_transcript_code_fallback());
    }

    #[test]
    fn markdown_preserves_blank_lines_in_highlighted_fenced_code() {
        let lines = render_content(
            "```rust\nlet first = 1;\n\nlet second = 2;\n```",
            style_transcript_normal(),
        );
        let text = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(text, vec!["let first = 1;", "", "let second = 2;"]);
    }

    #[test]
    fn markdown_highlights_unterminated_fenced_code_without_delimiters() {
        let lines = render_content("```rust\nlet value = 1;", style_transcript_normal());

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "let value = 1;");
        assert!(
            foregrounds(&lines[0])
                .into_iter()
                .collect::<HashSet<_>>()
                .len()
                >= 2
        );
    }

    #[test]
    fn plain_text_uses_base_style() {
        let base_style = Style::default().fg(Color::Cyan);
        let lines = render_content("cargo test -p cowboy", base_style);

        assert_eq!(lines[0].to_string(), "cargo test -p cowboy");
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].style, base_style);
    }

    #[test]
    fn empty_markdown_returns_one_empty_line() {
        let lines = render_content("", style_transcript_normal());

        assert_eq!(lines, vec![Line::from("")]);
    }
}
