use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::controls::chrome::{METADATA_SEPARATOR, truncate_to_display_width};
use super::styles::{
    style_accent, style_border, style_error, style_success, style_transcript_metadata,
    style_transcript_normal, style_transcript_plan, style_transcript_prompt,
    style_transcript_thought, style_transcript_tool_pending, style_warning,
};

pub(super) const DEFAULT_CARD_WIDTH: usize = 80;
const MIN_CARD_WIDTH: usize = 2;
const SECTION_BODY_LIMIT: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CardTone {
    Neutral,
    Accent,
    Success,
    Warning,
    Error,
    Thought,
    Prompt,
    Plan,
    Tool,
}

impl CardTone {
    pub(super) fn title_style(self) -> Style {
        match self {
            Self::Neutral => style_transcript_normal(),
            Self::Accent => style_accent(),
            Self::Success => style_success(),
            Self::Warning => style_warning(),
            Self::Error => style_error(),
            Self::Thought => style_transcript_thought(),
            Self::Prompt => style_transcript_prompt(),
            Self::Plan => style_transcript_plan(),
            Self::Tool => style_transcript_tool_pending(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CardMetadata {
    text: String,
}

impl CardMetadata {
    pub(super) fn step(step: impl AsRef<str>) -> Self {
        Self {
            text: format!("↳ {}", step.as_ref()),
        }
    }

    pub(super) fn run(run_id: impl AsRef<str>) -> Self {
        Self {
            text: format!(
                "▶ {}",
                super::controls::chrome::short_run_id(run_id.as_ref())
            ),
        }
    }

    pub(super) fn workflow(workflow: impl AsRef<str>) -> Self {
        Self {
            text: format!("⎇ {}", workflow.as_ref()),
        }
    }

    #[cfg(test)]
    pub(super) fn tasks(count: usize) -> Self {
        Self {
            text: format!("◷ {count}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CardSection {
    label: Option<String>,
    lines: Vec<Line<'static>>,
    max_lines: usize,
}

impl CardSection {
    pub(super) fn body(lines: Vec<Line<'static>>) -> Self {
        Self {
            label: None,
            lines,
            max_lines: SECTION_BODY_LIMIT,
        }
    }

    pub(super) fn named(label: impl Into<String>, lines: Vec<Line<'static>>) -> Self {
        Self {
            label: Some(label.into()),
            lines,
            max_lines: SECTION_BODY_LIMIT,
        }
    }

    pub(super) fn capped(mut self, max_lines: usize) -> Self {
        self.max_lines = max_lines;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Card {
    status: &'static str,
    title: String,
    tone: CardTone,
    title_prefix: Vec<String>,
    metadata: Vec<CardMetadata>,
    tool_marker: bool,
    sections: Vec<CardSection>,
}

impl Card {
    pub(super) fn new(status: &'static str, title: impl Into<String>, tone: CardTone) -> Self {
        Self {
            status,
            title: title.into(),
            tone,
            title_prefix: Vec::new(),
            metadata: Vec::new(),
            tool_marker: false,
            sections: Vec::new(),
        }
    }

    pub(super) fn metadata(mut self, metadata: impl IntoIterator<Item = CardMetadata>) -> Self {
        self.metadata = metadata.into_iter().collect();
        self
    }

    pub(super) fn title_prefix(mut self, text: impl Into<String>) -> Self {
        self.title_prefix.push(text.into());
        self
    }

    pub(super) fn tool_marker(mut self) -> Self {
        self.tool_marker = true;
        self
    }

    pub(super) fn section(mut self, section: CardSection) -> Self {
        self.sections.push(section);
        self
    }

    pub(super) fn render(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(MIN_CARD_WIDTH);
        let interior_width = width.saturating_sub(2);
        let rendered_sections = self
            .sections
            .iter()
            .map(|section| (section, section_wrapped_lines(section, interior_width)))
            .collect::<Vec<_>>();
        let has_content = rendered_sections
            .iter()
            .any(|(_, wrapped)| !wrapped.is_empty());
        let mut rows = Vec::new();
        rows.push(self.title_line(width));

        if !has_content {
            return rows;
        }

        rows.push(border_line('╭', '╮', width));

        for (section, wrapped) in rendered_sections {
            if let Some(label) = section.label.as_deref() {
                rows.push(section_divider(label, width));
            }

            rows.extend(
                wrapped
                    .into_iter()
                    .map(|line| framed_body_line(line, width)),
            );
        }

        rows.push(border_line('╰', '╯', width));
        rows
    }

    pub(super) fn plain_text(&self) -> String {
        self.render(DEFAULT_CARD_WIDTH)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn title_line(&self, width: usize) -> Line<'static> {
        let marker = if self.tool_marker { " • " } else { " " };
        let leading = format!("{}{}{}", self.status, marker, self.title);
        let mut parts = Vec::with_capacity(self.title_prefix.len() + self.metadata.len() + 1);
        parts.extend(self.title_prefix.iter().cloned());
        parts.push(leading);
        parts.extend(self.metadata.iter().map(|metadata| metadata.text.clone()));
        let title = truncate_to_display_width(parts.join(METADATA_SEPARATOR), width);
        Line::from(Span::styled(title, self.tone.title_style()))
    }
}

fn section_wrapped_lines(section: &CardSection, interior_width: usize) -> Vec<Line<'static>> {
    if interior_width == 0 {
        return Vec::new();
    }

    let wrapped = section
        .lines
        .iter()
        .cloned()
        .flat_map(|line| wrap_line(line, interior_width))
        .collect::<Vec<_>>();
    let truncated = wrapped.len() > section.max_lines;
    let mut lines = wrapped
        .into_iter()
        .take(section.max_lines)
        .collect::<Vec<_>>();

    if truncated {
        let omitted = section
            .lines
            .iter()
            .cloned()
            .flat_map(|line| wrap_line(line, interior_width))
            .count()
            .saturating_sub(section.max_lines);
        let marker = truncate_to_display_width(format!("… {omitted} more rows"), interior_width);
        lines.push(Line::from(Span::styled(
            marker,
            style_transcript_metadata(),
        )));
    }

    lines
}

fn border_line(left: char, right: char, width: usize) -> Line<'static> {
    if width <= 1 {
        return Line::from(Span::styled(left.to_string(), style_border()));
    }

    Line::from(Span::styled(
        format!("{left}{}{right}", "─".repeat(width.saturating_sub(2))),
        style_border(),
    ))
}

fn section_divider(label: &str, width: usize) -> Line<'static> {
    if width <= 2 {
        return border_line('├', '┤', width);
    }

    let interior_width = width.saturating_sub(2);
    let label = truncate_to_display_width(label, interior_width.saturating_sub(5));
    let label_width = UnicodeWidthStr::width(label.as_str());
    let prefix = if label.is_empty() {
        0
    } else {
        3.min(interior_width)
    };
    let suffix =
        interior_width.saturating_sub(prefix + label_width + if label.is_empty() { 0 } else { 2 });
    let mut text = String::from("├");
    text.push_str(&"─".repeat(prefix));
    if !label.is_empty() {
        text.push(' ');
        text.push_str(&label);
        text.push(' ');
    }
    text.push_str(&"─".repeat(suffix));
    text.push('┤');
    Line::from(Span::styled(text, style_border()))
}

fn framed_body_line(line: Line<'static>, width: usize) -> Line<'static> {
    if width <= 2 {
        return Line::from(Span::styled("│".repeat(width), style_border()));
    }

    let interior_width = width.saturating_sub(2);
    let content_width = line_width(&line);
    let padding = interior_width.saturating_sub(content_width);
    let mut spans = Vec::with_capacity(line.spans.len() + 3);
    spans.push(Span::styled("│", style_border()));
    spans.extend(line.spans);
    if padding > 0 {
        spans.push(Span::styled(" ".repeat(padding), style_transcript_normal()));
    }
    spans.push(Span::styled("│", style_border()));
    Line::from(spans)
}

fn wrap_line(line: Line<'static>, wrap_width: usize) -> Vec<Line<'static>> {
    let Line {
        spans,
        style,
        alignment,
    } = line;
    let mut rows = Vec::new();
    let mut row_spans = Vec::new();
    let mut row_width: usize = 0;

    for span in spans {
        let span_style = span.style;
        let mut segment = String::new();
        for ch in span.content.chars() {
            if ch == '\n' {
                push_span(&mut row_spans, &mut segment, span_style);
                push_visual_row(&mut rows, &mut row_spans, style, alignment);
                row_width = 0;
                continue;
            }

            let ch_width = ch.width().unwrap_or(0);
            if ch_width > 0 && row_width > 0 && row_width.saturating_add(ch_width) > wrap_width {
                push_span(&mut row_spans, &mut segment, span_style);
                push_visual_row(&mut rows, &mut row_spans, style, alignment);
                row_width = 0;
            }

            segment.push(ch);
            row_width = row_width.saturating_add(ch_width);
        }
        push_span(&mut row_spans, &mut segment, span_style);
    }

    push_visual_row(&mut rows, &mut row_spans, style, alignment);
    rows
}

fn push_span(spans: &mut Vec<Span<'static>>, segment: &mut String, style: Style) {
    if segment.is_empty() {
        return;
    }

    spans.push(Span::styled(std::mem::take(segment), style));
}

fn push_visual_row(
    rows: &mut Vec<Line<'static>>,
    spans: &mut Vec<Span<'static>>,
    style: Style,
    alignment: Option<ratatui::layout::Alignment>,
) {
    let mut row = Line::from(std::mem::take(spans));
    row.style = style;
    row.alignment = alignment;
    rows.push(row);
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_rounded_card_chrome_and_title_metadata() {
        let rows = Card::new("●", "Bash cargo test", CardTone::Tool)
            .tool_marker()
            .metadata([
                CardMetadata::step("implement"),
                CardMetadata::run("run-170dc431-abc"),
                CardMetadata::workflow("bugfix"),
                CardMetadata::tasks(1),
            ])
            .section(CardSection::named(
                "Output",
                vec![Line::from("running 23 tests")],
            ))
            .render(80);
        let text = rows
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("● • Bash cargo test"), "{text}");
        assert!(
            text.contains("↳ implement · ▶ 170dc431 · ⎇ bugfix · ◷ 1"),
            "{text}"
        );
        assert!(text.contains("╭"), "{text}");
        assert!(text.contains("╮"), "{text}");
        assert!(text.contains("╰"), "{text}");
        assert!(text.contains("╯"), "{text}");
        assert!(text.contains("├─── Output"), "{text}");
        assert!(!text.contains("step="), "{text}");
        assert!(!text.contains("run="), "{text}");
        assert!(!text.contains("workflow="), "{text}");
        assert!(!text.contains("tasks="), "{text}");
    }

    #[test]
    fn wide_cards_expand_to_available_width() {
        let rows = Card::new("●", "Wide card", CardTone::Accent)
            .section(CardSection::body(vec![Line::from("content")]))
            .render(120);

        let border_width = UnicodeWidthStr::width(rows[1].to_string().as_str());
        assert_eq!(
            border_width, 120,
            "card border should consume the available transcript width"
        );
    }

    #[test]
    fn renders_empty_card_without_border_chrome() {
        let rows = Card::new("●", "Idle tool", CardTone::Tool).render(80);
        let text = rows
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("● Idle tool"), "{text}");
        assert!(!text.contains('╭'), "{text}");
        assert!(!text.contains('╮'), "{text}");
        assert!(!text.contains('╰'), "{text}");
        assert!(!text.contains('╯'), "{text}");
        assert!(!text.contains('│'), "{text}");
    }

    #[test]
    fn renders_empty_body_section_without_border_chrome() {
        let rows = Card::new("●", "Idle tool", CardTone::Tool)
            .section(CardSection::body(Vec::new()))
            .render(80);
        let text = rows
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("● Idle tool"), "{text}");
        assert!(!text.contains('╭'), "{text}");
        assert!(!text.contains('╮'), "{text}");
        assert!(!text.contains('╰'), "{text}");
        assert!(!text.contains('╯'), "{text}");
        assert!(!text.contains('│'), "{text}");
    }

    #[test]
    fn truncates_titles_safely_and_keeps_rows_within_width() {
        let width = 18;
        let rows = Card::new("●", "读取 very long title", CardTone::Accent)
            .metadata([CardMetadata::step("实现实现实现")])
            .section(CardSection::body(vec![Line::from("abcdef ghijkl mnopqr")]))
            .render(width);

        assert!(rows[0].to_string().contains('…'), "{:?}", rows[0]);
        assert!(
            rows.iter()
                .all(|line| UnicodeWidthStr::width(line.to_string().as_str()) <= width)
        );
    }

    #[test]
    fn wraps_body_lines_inside_borders_and_marks_truncation() {
        let rows = Card::new("✓", "Done", CardTone::Success)
            .section(
                CardSection::named(
                    "Body",
                    vec![
                        Line::from("1234567890"),
                        Line::from("line 2"),
                        Line::from("line 3"),
                    ],
                )
                .capped(2),
            )
            .render(8);
        let text = rows
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("│123456│"), "{text}");
        assert!(text.contains("│7890  │"), "{text}");
        assert!(text.contains("… 2 m…"), "{text}");
        assert!(
            rows.iter()
                .all(|line| UnicodeWidthStr::width(line.to_string().as_str()) <= 8)
        );
    }

    #[test]
    fn caps_sections_after_visual_wrapping() {
        let rows = Card::new("●", "Long output", CardTone::Tool)
            .section(
                CardSection::named("Output", vec![Line::from("abcdefghijklmnopqrstuvwxyz")])
                    .capped(2),
            )
            .render(10);
        let text = rows
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("│abcdefgh│"), "{text}");
        assert!(text.contains("│ijklmnop│"), "{text}");
        assert!(text.contains("… 2 mor…"), "{text}");
        assert!(!text.contains("qrstuvwx"), "{text}");
        assert!(rows.len() <= 7, "{rows:?}");
        assert!(
            rows.iter()
                .all(|line| UnicodeWidthStr::width(line.to_string().as_str()) <= 10)
        );
    }
}
