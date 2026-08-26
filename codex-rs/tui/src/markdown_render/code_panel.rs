use crate::render::line_utils::line_to_static;
use crate::width::display_width;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;

/// Hard-wraps a syntax-highlighted code row without changing or discarding source whitespace.
///
/// Visual wrapping is independent from `/copy-code`, which continues to use the raw Markdown
/// source. Keeping this helper span-aware preserves syntax colors across continuation rows.
pub(crate) fn hard_wrap_code_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    if line.width() <= width {
        return vec![line_to_static(line)];
    }
    let mut rows = vec![Line::default().style(line.style)];
    let mut row_width: usize = 0;

    for span in &line.spans {
        for grapheme in span.content.graphemes(/*is_extended*/ true) {
            let grapheme_width = display_width(grapheme);
            if row_width > 0 && row_width.saturating_add(grapheme_width) > width {
                rows.push(Line::default().style(line.style));
                row_width = 0;
            }
            let Some(row) = rows.last_mut() else {
                return rows;
            };
            if let Some(last) = row.spans.last_mut()
                && last.style == span.style
            {
                last.content.to_mut().push_str(grapheme);
            } else {
                row.push_span(Span::styled(grapheme.to_string(), span.style));
            }
            row_width = row_width.saturating_add(grapheme_width);
        }
    }

    rows
}
