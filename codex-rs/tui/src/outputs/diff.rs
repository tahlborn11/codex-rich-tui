//! File-oriented rendering for the Outputs change browser.
//!
//! This deliberately keeps the original patch in [`FileDiff::source`].  The
//! renderer works from a parsed view, so copying a file never copies a
//! prettified or terminal-sanitized version of the diff.

use std::borrow::Cow;
use std::path::Path;

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::diff_render::DiffLineType;
use crate::diff_render::DiffRenderStyleContext;
use crate::diff_render::current_diff_render_style_context;
use crate::diff_render::output_diff_gutter_style;
use crate::diff_render::output_diff_line_style;
use crate::diff_render::output_diff_sign_style;
use crate::diff_render::style_output_diff_spans;
use crate::diff_render::wrap_styled_spans;
use crate::history_cell::sanitize_user_text;
use crate::render::highlight::exceeds_highlight_limits;
use crate::render::highlight::highlight_code_to_styled_spans;

/// A single file section from a unified patch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FileDiff {
    /// Stable within the patch, suitable for a list selection key.
    pub id: String,
    /// A display-only path or description. Terminal controls are removed when rendered.
    pub title: String,
    /// Exact source bytes represented as UTF-8 text, retained for copy.
    pub source: String,
}

impl FileDiff {
    pub(super) fn is_new(&self) -> bool {
        let prelude = self.source.split("@@ ").next().unwrap_or(&self.source);
        prelude.lines().any(|line| {
            line == "--- /dev/null"
                || line.starts_with("new file mode ")
                || line.starts_with("Untracked empty file:")
        })
    }
}

#[derive(Clone, Debug)]
enum PatchLine {
    Hunk(String),
    Context {
        old: Option<usize>,
        new: Option<usize>,
        text: String,
    },
    Delete {
        old: Option<usize>,
        text: String,
    },
    Add {
        new: Option<usize>,
        text: String,
    },
    Meta(String),
}

const SIDE_BY_SIDE_MIN_WIDTH: u16 = 90;
const MIN_NUMBER_WIDTH: usize = 4;

/// Split a unified patch into file sections.
///
/// We only split at actual line-leading Git headers.  In particular, `---` and
/// `+++` in a hunk remain content rather than becoming a false file boundary.
pub(super) fn parse(patch: &str) -> Vec<FileDiff> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in patch.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        let bare = bare.strip_suffix('\r').unwrap_or(bare);
        if bare.starts_with("diff --git ")
            || (bare.starts_with("diff -- ") && !bare.starts_with("diff --git "))
            || bare.starts_with("Untracked empty file:")
        {
            starts.push((offset, bare.to_owned()));
        }
        offset += line.len();
    }
    // `split_inclusive` does not yield a final empty line, which is exactly
    // what we want. A header without a trailing LF was still visited above.
    let sections = starts
        .iter()
        .enumerate()
        .map(|(index, (start, header))| {
            let end = starts.get(index + 1).map_or(patch.len(), |(next, _)| *next);
            let source = patch[*start..end].to_owned();
            FileDiff {
                id: format!("file-{index}"),
                title: file_title(header, &source),
                source,
            }
        })
        .collect::<Vec<_>>();
    let mut files: Vec<FileDiff> = Vec::new();
    let mut positions: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for section in sections {
        if let Some(&index) = positions.get(&section.title) {
            files[index].source.push_str(&section.source);
        } else {
            positions.insert(section.title.clone(), files.len());
            files.push(section);
        }
    }
    files
}

fn file_title(header: &str, source: &str) -> String {
    let prelude = source.split("@@ ").next().unwrap_or(source);
    if let Some(path) = prelude
        .lines()
        .find_map(|line| line.strip_prefix("rename to "))
    {
        return path.to_owned();
    }
    if let Some(path) = prelude.lines().find_map(|line| line.strip_prefix("+++ "))
        && path != "/dev/null"
    {
        return clean_path(path);
    }
    if let Some(path) = prelude.lines().find_map(|line| line.strip_prefix("--- "))
        && path != "/dev/null"
    {
        return clean_path(path);
    }
    if let Some(path) = header.strip_prefix("Untracked empty file:") {
        return path.trim().to_owned();
    }
    if let Some(paths) = header.strip_prefix("diff --git ") {
        return git_header_path(paths);
    }
    header
        .strip_prefix("diff -- ")
        .unwrap_or(header)
        .trim()
        .to_owned()
}

fn git_header_path(paths: &str) -> String {
    // Do not decode Git's C-style escapes: the displayed spelling is useful,
    // and decoding malformed input would make a renderer failure possible.
    // We do recognize quoted tokens so a path containing a space is not cut
    // at its final word.
    let mut quoted = Vec::new();
    let mut rest = paths;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let mut escaped = false;
        let mut end = None;
        for (index, ch) in after.char_indices() {
            if ch == '"' && !escaped {
                end = Some(index);
                break;
            }
            escaped = ch == '\\' && !escaped;
            if ch != '\\' {
                escaped = false;
            }
        }
        let Some(end) = end else { break };
        quoted.push(&after[..end]);
        rest = &after[end + 1..];
    }
    if let Some(path) = quoted.last() {
        clean_path(path)
    } else {
        // Git leaves plain spaces unquoted. Keep the complete description when
        // the two paths cannot be separated unambiguously.
        let parts = paths.split_whitespace().collect::<Vec<_>>();
        if parts.len() == 2 {
            clean_path(parts[1])
        } else {
            paths.to_owned()
        }
    }
}

fn clean_path(path: &str) -> String {
    let path = path
        .strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
        .unwrap_or(path);
    path.strip_prefix("b/")
        .or_else(|| path.strip_prefix("a/"))
        .unwrap_or(path)
        .to_owned()
}

/// Render one file. Wide panes use aligned old/new columns; narrow panes keep
/// a conventional unified diff so no source is hidden.
pub(super) fn render(file: &FileDiff, width: u16) -> Text<'static> {
    render_with_style_context(file, width, current_diff_render_style_context())
}

fn render_with_style_context(
    file: &FileDiff,
    width: u16,
    style_context: DiffRenderStyleContext,
) -> Text<'static> {
    let parsed = parse_lines(&file.source);
    let syntax = syntax_for_hunks(&file.title, &parsed);
    if width >= SIDE_BY_SIDE_MIN_WIDTH {
        render_side_by_side(&file.title, &parsed, &syntax, width, style_context)
    } else {
        render_unified(&file.title, &parsed, &syntax, width, style_context)
    }
}

type SyntaxPair = (Option<Vec<Span<'static>>>, Option<Vec<Span<'static>>>);

/// Highlight the old and new sides of each hunk independently.  A deletion
/// must not advance the new-file parser, and an addition must not advance the
/// old-file parser; otherwise multiline strings/comments after a replacement
/// acquire the wrong syntax colors.  Starting fresh per hunk matches the
/// visual discontinuity between hunk sections.
fn syntax_for_hunks(title: &str, parsed: &[PatchLine]) -> Vec<SyntaxPair> {
    let mut result = vec![(None, None); parsed.len()];
    let Some(language) = Path::new(title).extension().and_then(|ext| ext.to_str()) else {
        return result;
    };
    let (total_bytes, total_lines) = parsed.iter().fold((0usize, 0usize), |acc, line| {
        let text = match line {
            PatchLine::Context { text, .. }
            | PatchLine::Delete { text, .. }
            | PatchLine::Add { text, .. } => text,
            PatchLine::Hunk(_) | PatchLine::Meta(_) => return acc,
        };
        (acc.0.saturating_add(text.len()), acc.1.saturating_add(1))
    });
    if exceeds_highlight_limits(total_bytes, total_lines) {
        return result;
    }
    let mut start = 0;
    while start < parsed.len() {
        while start < parsed.len() && !matches!(parsed[start], PatchLine::Hunk(_)) {
            start += 1;
        }
        if start == parsed.len() {
            break;
        }
        let end = parsed[start + 1..]
            .iter()
            .position(|line| matches!(line, PatchLine::Hunk(_)))
            .map_or(parsed.len(), |offset| start + 1 + offset);
        let mut old_text = Vec::new();
        let mut new_text = Vec::new();
        let mut old_indices = Vec::new();
        let mut new_indices = Vec::new();
        for (index, line) in parsed.iter().enumerate().take(end).skip(start + 1) {
            match line {
                PatchLine::Context { text, .. } => {
                    old_indices.push(index);
                    new_indices.push(index);
                    old_text.push(display(text));
                    new_text.push(display(text));
                }
                PatchLine::Delete { text, .. } => {
                    old_indices.push(index);
                    old_text.push(display(text));
                }
                PatchLine::Add { text, .. } => {
                    new_indices.push(index);
                    new_text.push(display(text));
                }
                PatchLine::Hunk(_) | PatchLine::Meta(_) => {}
            }
        }
        let old = highlight_source_lines(&old_text, language);
        let new = highlight_source_lines(&new_text, language);
        for (source_index, parsed_index) in old_indices.into_iter().enumerate() {
            result[parsed_index].0 = old.get(source_index).cloned();
        }
        for (source_index, parsed_index) in new_indices.into_iter().enumerate() {
            result[parsed_index].1 = new.get(source_index).cloned();
        }
        start = end;
    }
    result
}

fn highlight_source_lines(lines: &[String], language: &str) -> Vec<Vec<Span<'static>>> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut source = lines.join("\n");
    source.push('\n');
    highlight_code_to_styled_spans(&source, language).unwrap_or_default()
}

fn parse_lines(source: &str) -> Vec<PatchLine> {
    let mut lines = Vec::new();
    let mut in_hunk = false;
    let mut old = 0usize;
    let mut new = 0usize;
    for raw in source.lines() {
        if raw.starts_with("diff --") || raw.starts_with("Untracked empty file:") {
            in_hunk = false;
        }
        // These lines identify the patch machinery rather than a change the
        // viewer needs to present. The file title and Old/New headings already
        // supply the useful context. Do this only before a hunk: `---` and
        // `+++` are ordinary deleted/added content when they appear in one.
        if raw.starts_with("diff --") || (!in_hunk && is_routine_file_header(raw)) {
            continue;
        }
        if let Some((o, n)) = parse_hunk(raw) {
            old = o;
            new = n;
            in_hunk = true;
            lines.push(PatchLine::Hunk(raw.to_owned()));
            continue;
        }
        if in_hunk {
            if let Some(text) = raw.strip_prefix(' ') {
                lines.push(PatchLine::Context {
                    old: Some(old),
                    new: Some(new),
                    text: text.to_owned(),
                });
                old = old.saturating_add(1);
                new = new.saturating_add(1);
                continue;
            }
            if let Some(text) = raw.strip_prefix('-') {
                lines.push(PatchLine::Delete {
                    old: Some(old),
                    text: text.to_owned(),
                });
                old = old.saturating_add(1);
                continue;
            }
            if let Some(text) = raw.strip_prefix('+') {
                lines.push(PatchLine::Add {
                    new: Some(new),
                    text: text.to_owned(),
                });
                new = new.saturating_add(1);
                continue;
            }
            if raw.starts_with("\\ No newline at end of file") {
                lines.push(PatchLine::Meta(raw.to_owned()));
                continue;
            }
        }
        lines.push(PatchLine::Meta(raw.to_owned()));
    }
    lines
}

fn is_routine_file_header(line: &str) -> bool {
    line.starts_with("index ") || line.starts_with("--- ") || line.starts_with("+++ ")
}

fn parse_hunk(line: &str) -> Option<(usize, usize)> {
    let body = line.strip_prefix("@@ ")?;
    let (ranges, _) = body.split_once(" @@")?;
    let mut ranges = ranges.split_whitespace();
    let old = parse_range(ranges.next()?.strip_prefix('-')?)?;
    let new = parse_range(ranges.next()?.strip_prefix('+')?)?;
    Some((old, new))
}

fn parse_range(value: &str) -> Option<usize> {
    value
        .split_once(',')
        .map_or_else(|| value.parse().ok(), |(number, _)| number.parse().ok())
}

fn display(input: &str) -> String {
    sanitize_user_text(Cow::Borrowed(input)).replace('\t', "    ")
}

fn render_unified(
    title: &str,
    parsed: &[PatchLine],
    syntax: &[SyntaxPair],
    width: u16,
    style_context: DiffRenderStyleContext,
) -> Text<'static> {
    let mut out = vec![Line::from(Span::styled(
        display(title),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    let gutter = number_width(parsed);
    let content_width = usize::from(width.max(12))
        .saturating_sub(gutter * 2 + 4)
        .max(1);
    for (index, line) in parsed.iter().enumerate() {
        match line {
            PatchLine::Hunk(value) => out.push(Line::from(Span::styled(
                display(value),
                Style::default().fg(Color::Yellow),
            ))),
            PatchLine::Meta(value) => out.extend(
                wrap(&display(value), content_width)
                    .into_iter()
                    .map(|part| {
                        Line::from(Span::styled(
                            part,
                            Style::default().add_modifier(Modifier::DIM),
                        ))
                    }),
            ),
            PatchLine::Context { old, new, text } => out.extend(unified_rows(
                (*old, *new, ' '),
                text,
                content_width,
                DiffLineType::Context,
                syntax.get(index).and_then(|pair| pair.1.as_deref()),
                gutter,
                style_context,
            )),
            PatchLine::Delete { old, text } => out.extend(unified_rows(
                (*old, None, '-'),
                text,
                content_width,
                DiffLineType::Delete,
                syntax.get(index).and_then(|pair| pair.0.as_deref()),
                gutter,
                style_context,
            )),
            PatchLine::Add { new, text } => out.extend(unified_rows(
                (None, *new, '+'),
                text,
                content_width,
                DiffLineType::Insert,
                syntax.get(index).and_then(|pair| pair.1.as_deref()),
                gutter,
                style_context,
            )),
        }
    }
    Text::from(out)
}

fn unified_rows(
    position: (Option<usize>, Option<usize>, char),
    text: &str,
    width: usize,
    kind: DiffLineType,
    syntax: Option<&[Span<'static>]>,
    gutter: usize,
    style_context: DiffRenderStyleContext,
) -> Vec<Line<'static>> {
    let (old, new, sign) = position;
    styled_wrap(text, syntax, width, kind, style_context)
        .into_iter()
        .enumerate()
        .map(|(idx, part)| {
            let left = if idx == 0 {
                old.map_or_else(String::new, |n| n.to_string())
            } else {
                String::new()
            };
            let right = if idx == 0 {
                new.map_or_else(String::new, |n| n.to_string())
            } else {
                String::new()
            };
            let mut spans = vec![
                Span::styled(
                    format!("{left:>gutter$} {right:>gutter$} "),
                    output_diff_gutter_style(kind, style_context),
                ),
                Span::styled(
                    sign.to_string(),
                    output_diff_sign_style(kind, style_context),
                ),
                Span::styled(" ", output_diff_line_style(kind, style_context)),
            ];
            spans.extend(spans_with_padding(part, width, kind, style_context));
            Line::from(spans)
        })
        .collect()
}

fn render_side_by_side(
    title: &str,
    parsed: &[PatchLine],
    syntax: &[SyntaxPair],
    width: u16,
    style_context: DiffRenderStyleContext,
) -> Text<'static> {
    let mut out = vec![Line::from(Span::styled(
        display(title),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    let gutter = number_width(parsed);
    let content_width = (usize::from(width).saturating_sub(gutter * 2 + 9) / 2).max(1);
    let pane_width = gutter + 3 + content_width;
    out.push(Line::from(vec![
        Span::styled(
            format!("{:<pane_width$}", "Old"),
            Style::default().fg(Color::Red),
        ),
        Span::raw(" │ "),
        Span::styled("New", Style::default().fg(Color::Green)),
    ]));
    let mut index = 0;
    while index < parsed.len() {
        match &parsed[index] {
            PatchLine::Hunk(value) => {
                out.push(Line::from(Span::styled(
                    display(value),
                    Style::default().fg(Color::Yellow),
                )));
                index += 1;
            }
            PatchLine::Meta(value) => {
                out.extend(
                    wrap(&display(value), usize::from(width).saturating_sub(2))
                        .into_iter()
                        .map(|part| {
                            Line::from(Span::styled(
                                part,
                                Style::default().add_modifier(Modifier::DIM),
                            ))
                        }),
                );
                index += 1;
            }
            PatchLine::Context { old, new, text } => {
                out.extend(side_rows(
                    (
                        *old,
                        text,
                        DiffLineType::Context,
                        syntax.get(index).and_then(|pair| pair.0.as_deref()),
                    ),
                    (
                        *new,
                        text,
                        DiffLineType::Context,
                        syntax.get(index).and_then(|pair| pair.1.as_deref()),
                    ),
                    content_width,
                    gutter,
                    style_context,
                ));
                index += 1;
            }
            PatchLine::Delete { .. } | PatchLine::Add { .. } => {
                let start = index;
                let mut no_newline = Vec::new();
                while index < parsed.len() && is_change_or_newline_marker(&parsed[index]) {
                    if let PatchLine::Meta(value) = &parsed[index] {
                        no_newline.push(value.clone());
                    }
                    index += 1;
                }
                let mut deleted = Vec::new();
                let mut added = Vec::new();
                for (offset, change) in parsed[start..index].iter().enumerate() {
                    let syntax_pair = syntax.get(start + offset);
                    match change {
                        PatchLine::Delete { old, text } => deleted.push((
                            *old,
                            text.as_str(),
                            syntax_pair.and_then(|pair| pair.0.as_deref()),
                        )),
                        PatchLine::Add { new, text } => added.push((
                            *new,
                            text.as_str(),
                            syntax_pair.and_then(|pair| pair.1.as_deref()),
                        )),
                        PatchLine::Meta(_) => {}
                        _ => unreachable!("change run contains only additions and deletions"),
                    }
                }
                let rows = deleted.len().max(added.len());
                for row in 0..rows {
                    let left = deleted
                        .get(row)
                        .map(|(n, text, spans)| (*n, *text, DiffLineType::Delete, *spans))
                        .unwrap_or((None, "", DiffLineType::Context, None));
                    let right = added
                        .get(row)
                        .map(|(n, text, spans)| (*n, *text, DiffLineType::Insert, *spans))
                        .unwrap_or((None, "", DiffLineType::Context, None));
                    out.extend(side_rows(left, right, content_width, gutter, style_context));
                }
                out.extend(no_newline.into_iter().map(|value| {
                    Line::from(Span::styled(
                        display(&value),
                        Style::default().add_modifier(Modifier::DIM),
                    ))
                }));
            }
        }
    }
    Text::from(out)
}

fn side_rows(
    left_input: (Option<usize>, &str, DiffLineType, Option<&[Span<'static>]>),
    right_input: (Option<usize>, &str, DiffLineType, Option<&[Span<'static>]>),
    width: usize,
    gutter: usize,
    style_context: DiffRenderStyleContext,
) -> Vec<Line<'static>> {
    let left = styled_wrap(
        left_input.1,
        left_input.3,
        width,
        left_input.2,
        style_context,
    );
    let right = styled_wrap(
        right_input.1,
        right_input.3,
        width,
        right_input.2,
        style_context,
    );
    let rows = left.len().max(right.len()).max(1);
    (0..rows)
        .map(|index| {
            let (left_text, left_kind) = left.get(index).cloned().map_or_else(
                || (Vec::new(), DiffLineType::Context),
                |text| (text, left_input.2),
            );
            let (right_text, right_kind) = right.get(index).cloned().map_or_else(
                || (Vec::new(), DiffLineType::Context),
                |text| (text, right_input.2),
            );
            let left_number = if index == 0 {
                format_line_number(left_input.0)
            } else {
                String::new()
            };
            let right_number = if index == 0 {
                format_line_number(right_input.0)
            } else {
                String::new()
            };
            let mut spans = vec![Span::styled(
                format!("{left_number:>gutter$} │ "),
                output_diff_gutter_style(left_kind, style_context),
            )];
            spans.extend(spans_with_padding(
                left_text,
                width,
                left_kind,
                style_context,
            ));
            spans.push(Span::raw(" │ "));
            spans.push(Span::styled(
                format!("{right_number:>gutter$} │ "),
                output_diff_gutter_style(right_kind, style_context),
            ));
            spans.extend(spans_with_padding(
                right_text,
                width,
                right_kind,
                style_context,
            ));
            Line::from(spans)
        })
        .collect()
}

fn format_line_number(number: Option<usize>) -> String {
    number.map_or_else(String::new, |number| number.to_string())
}

fn is_change_or_newline_marker(line: &PatchLine) -> bool {
    matches!(line, PatchLine::Delete { .. } | PatchLine::Add { .. })
        || matches!(line, PatchLine::Meta(value) if value.starts_with("\\ No newline at end of file"))
}

fn number_width(parsed: &[PatchLine]) -> usize {
    parsed
        .iter()
        .flat_map(|line| match line {
            PatchLine::Context { old, new, .. } => [*old, *new],
            PatchLine::Delete { old, .. } => [*old, None],
            PatchLine::Add { new, .. } => [None, *new],
            PatchLine::Hunk(_) | PatchLine::Meta(_) => [None, None],
        })
        .flatten()
        .map(|number| number.to_string().len())
        .max()
        .unwrap_or(MIN_NUMBER_WIDTH)
        .max(MIN_NUMBER_WIDTH)
}
fn styled_wrap(
    text: &str,
    syntax: Option<&[Span<'static>]>,
    width: usize,
    kind: DiffLineType,
    style_context: DiffRenderStyleContext,
) -> Vec<Vec<Span<'static>>> {
    let styled = style_output_diff_spans(kind, syntax, text, style_context)
        .into_iter()
        .map(|span| Span::styled(display(span.content.as_ref()), span.style))
        .collect::<Vec<_>>();
    wrap_styled_spans(&grapheme_aligned_spans(styled), width.max(1))
}

// Syntax token boundaries may bisect a combining sequence or emoji. Move
// those boundaries to complete graphemes before the shared span wrapper runs.
fn grapheme_aligned_spans(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let text = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let mut source = spans.iter();
    let mut end = 0usize;
    let mut style = Style::default();
    let mut aligned: Vec<Span<'static>> = Vec::new();
    for (start, grapheme) in text.grapheme_indices(true) {
        while start >= end {
            let Some(span) = source.next() else {
                break;
            };
            end += span.content.len();
            style = span.style;
        }
        if let Some(previous) = aligned
            .last_mut()
            .filter(|previous| previous.style == style)
        {
            previous.content.to_mut().push_str(grapheme);
        } else {
            aligned.push(Span::styled(grapheme.to_owned(), style));
        }
    }
    aligned
}

fn spans_with_padding(
    mut spans: Vec<Span<'static>>,
    width: usize,
    kind: DiffLineType,
    style_context: DiffRenderStyleContext,
) -> Vec<Span<'static>> {
    let used = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    let padding = width.saturating_sub(used);
    if padding > 0 {
        spans.push(Span::styled(
            " ".repeat(padding),
            output_diff_line_style(kind, style_context),
        ));
    }
    spans
}

/// Wrap on grapheme boundaries. This never truncates a long token and keeps
/// paired side-by-side rows aligned when one side needs additional rows.
fn wrap(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if value.is_empty() {
        return vec![String::new()];
    }
    let mut lines = vec![String::new()];
    let mut used = 0;
    for grapheme in value.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used > 0 && used + grapheme_width > width {
            lines.push(String::new());
            used = 0;
        }
        if let Some(line) = lines.last_mut() {
            line.push_str(grapheme);
        }
        used += grapheme_width;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_files_and_preserves_exact_source() {
        let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.rs b/b.rs\nBinary files a/b.rs and b/b.rs differ\n";
        let files = parse(patch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].title, "a.rs");
        assert_eq!(
            files
                .iter()
                .map(|file| file.source.as_str())
                .collect::<String>(),
            patch
        );
        assert_eq!(files[1].title, "b.rs");
    }

    #[test]
    fn recognizes_synthetic_empty_and_rename_sections() {
        let patch = "Untracked empty file: café.txt\ndiff -- src/lib.rs\nrename from old.rs\nrename to new.rs\n@@ -2,2 +2,3 @@\n keep\n--- code\n+++ code\n";
        let files = parse(patch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].title, "café.txt");
        assert_eq!(files[1].title, "new.rs");
        let rendered = render(&files[1], 120).to_string();
        assert!(rendered.contains("-- code"));
        assert!(rendered.contains("++ code"));
    }

    #[test]
    fn new_file_marker_uses_metadata_instead_of_added_content() {
        for (patch, expected) in [
            (
                "diff --git a/x b/x\nnew file mode 100644\nBinary files /dev/null and b/x differ\n",
                true,
            ),
            (
                "diff --git a/x b/x\n--- /dev/null\n+++ b/x\n@@ -0,0 +1 @@\n+new\n",
                true,
            ),
            ("Untracked empty file: x\n", true),
            (
                "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new file mode 100644\n",
                false,
            ),
            (
                "diff --git a/x b/x\ndeleted file mode 100644\n--- a/x\n+++ /dev/null\n",
                false,
            ),
        ] {
            assert_eq!(parse(patch)[0].is_new(), expected, "{patch}");
        }
    }

    #[test]
    fn keeps_quoted_paths_and_crlf_source_exact() {
        let patch = "diff --git \"a/old name.rs\" \"b/new name.rs\"\r\n--- /dev/null\r\n+++ \"b/new name.rs\"\r\n";
        let files = parse(patch);
        assert_eq!(files[0].title, "new name.rs");
        assert_eq!(files[0].source, patch);
    }

    #[test]
    fn side_by_side_pairs_add_before_delete_and_wraps_unicode() {
        let file = FileDiff {
            id: "x".into(),
            title: "x".into(),
            source: "diff -- x\n@@ -1 +1 @@\n+新しい長い行abcdef\n-old\n".into(),
        };
        let text = render(&file, 90);
        let rendered = text.to_string();
        assert!(rendered.contains("old"));
        assert!(rendered.contains("新しい"));
        assert!(text.lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(Color::Red) || span.style.bg.is_some())
        }));
        assert!(text.lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(Color::Green) || span.style.bg.is_some())
        }));
    }

    #[test]
    fn syntax_highlighting_keeps_old_and_new_hunk_state_independent() {
        let parsed = parse_lines(
            "@@ -1,3 +1,3 @@\n-/* comment starts\n+let introduced = 1;\n let still_code = true;\n */\n",
        );
        let syntax = syntax_for_hunks("example.rs", &parsed);
        let context = syntax
            .iter()
            .enumerate()
            .find(|(index, _)| matches!(&parsed[*index], PatchLine::Context { .. }))
            .map(|(_, pair)| pair)
            .expect("context line");
        let old = context.0.as_ref().expect("old source highlighted");
        let new = context.1.as_ref().expect("new source highlighted");
        assert_eq!(
            old.iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "let still_code = true;"
        );
        assert_eq!(
            new.iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "let still_code = true;"
        );
        assert_ne!(old, new, "the old source remains inside the block comment");
    }

    #[test]
    fn unknown_extension_keeps_plain_fallback_and_blank_changed_rows() {
        let parsed = parse_lines("@@ -1 +1 @@\n-\n+\n");
        let syntax = syntax_for_hunks("notes.unknown-extension", &parsed);
        assert!(
            syntax
                .iter()
                .all(|pair| pair.0.is_none() && pair.1.is_none())
        );
        let file = FileDiff {
            id: "blank".into(),
            title: "notes.unknown-extension".into(),
            source: "diff -- notes.unknown-extension\n@@ -1 +1 @@\n-\n+\n".into(),
        };
        let rendered = render(&file, 120);
        assert!(
            rendered
                .lines
                .iter()
                .any(|line| line.to_string().contains(" │ "))
        );
    }

    #[test]
    fn narrow_is_unified_and_no_newline_marker_is_visible() {
        let file = FileDiff {
            id: "x".into(),
            title: "x".into(),
            source: "diff -- x\n@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file\n".into(),
        };
        let text = render(&file, 40).to_string();
        assert!(text.contains(" - a"));
        assert!(text.contains(" + b"));
        assert!(text.contains("No newline"));
    }

    #[test]
    fn hides_routine_headers_in_unified_and_side_by_side_previews() {
        let source = "diff --git a/lib.rs b/lib.rs\nindex 1234567..89abcde 100644\n--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let file = parse(source).remove(0);

        for width in [60, 120] {
            let rendered = render(&file, width).to_string();
            assert!(rendered.contains("@@ -1 +1 @@"));
            assert!(rendered.contains("old"));
            assert!(rendered.contains("new"));
            assert!(!rendered.contains("diff --git"));
            assert!(!rendered.contains("index 1234567"));
            assert!(!rendered.contains("--- a/lib.rs"));
            assert!(!rendered.contains("+++ b/lib.rs"));
        }

        assert_eq!(file.source, source);
    }

    #[test]
    fn keeps_useful_prelude_metadata_and_in_hunk_header_like_content() {
        let source = "diff --git a/old.rs b/new.rs\nsimilarity index 98%\nrename from old.rs\nrename to new.rs\nold mode 100644\nnew mode 100755\nBinary files a/old.rs and b/new.rs differ\nnew file mode 100644\ndeleted file mode 100644\nindex 123..456 100644\n--- a/old.rs\n+++ b/new.rs\n@@ -1,2 +1,2 @@\n--- content\n+++ content\n\\ No newline at end of file\n";
        let file = parse(source).remove(0);

        for width in [60, 120] {
            let rendered = render(&file, width).to_string();
            for retained in [
                "similarity index 98%",
                "rename from old.rs",
                "rename to new.rs",
                "old mode 100644",
                "new mode 100755",
                "Binary files a/old.rs and b/new.rs differ",
                "new file mode 100644",
                "deleted file mode 100644",
                "No newline at end of file",
                "-- content",
                "++ content",
            ] {
                assert!(
                    rendered.contains(retained),
                    "missing {retained:?} at {width}"
                );
            }
            assert!(!rendered.contains("index 123..456"));
            assert!(!rendered.contains("--- a/old.rs"));
            assert!(!rendered.contains("+++ b/new.rs"));
        }
    }

    #[test]
    fn messages_without_headers_are_not_files() {
        assert!(parse("No changes\n").is_empty());
    }
    #[test]
    fn hunk_content_cannot_rename_the_file_and_large_numbers_do_not_overflow() {
        let source = format!(
            "diff -- src/code.rs\n@@ -{},1 +{},1 @@\n--- payload\n+++ payload\n",
            usize::MAX,
            usize::MAX
        );
        let files = parse(&source);
        assert_eq!(files[0].title, "src/code.rs");
        for width in [60, 90, 120] {
            let text = render(&files[0], width);
            assert!(text.to_string().contains("-- payload"));
            assert!(text.to_string().contains("++ payload"));
            for line in &text.lines {
                assert!(
                    line.width() <= usize::from(width),
                    "line exceeds {width}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn wrapped_replacements_keep_columns_aligned_and_strip_terminal_controls() {
        let source = format!(
            "diff -- x\n@@ -1 +1 @@\n-{}\n+\x1b[31mnew\n",
            "old界".repeat(30)
        );
        let file = parse(&source).remove(0);
        let text = render(&file, 90);
        let rows = text
            .lines
            .iter()
            .filter(|line| {
                line.spans
                    .iter()
                    .any(|span| span.style.fg == Some(Color::Red))
                    && line.to_string().contains('界')
            })
            .collect::<Vec<_>>();
        assert!(rows.len() > 1);
        for row in rows {
            assert!(row.width() <= 90);
            assert!(row.to_string().contains(" │ "));
        }
        assert!(!text.to_string().contains('\x1b'));
        assert_eq!(file.source, source);
    }

    #[test]
    fn deleted_files_and_empty_files_remain_separate_entries() {
        let patch = "diff --git a/deleted b/deleted\n--- a/deleted\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\nUntracked empty file: empty.txt\n";
        let files = parse(patch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].title, "deleted");
        assert_eq!(files[1].title, "empty.txt");
        assert_eq!(
            files
                .iter()
                .map(|file| file.source.as_str())
                .collect::<String>(),
            patch
        );
    }

    #[test]
    fn repeated_file_sections_are_grouped_without_treating_headers_as_edits() {
        let first = "diff -- x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+middle\n";
        let second = "diff -- x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-middle\n+final\n";
        let files = parse(&format!("{first}{second}"));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].source, format!("{first}{second}"));
        let parsed = parse_lines(&files[0].source);
        assert_eq!(
            parsed
                .iter()
                .filter(|line| matches!(line, PatchLine::Delete { .. }))
                .count(),
            2
        );
        assert_eq!(
            parsed
                .iter()
                .filter(|line| matches!(line, PatchLine::Add { .. }))
                .count(),
            2
        );
        for width in [60, 120] {
            let rendered = render(&files[0], width).to_string();
            assert!(rendered.contains("old"));
            assert!(rendered.contains("middle"));
            assert!(rendered.contains("final"));
            assert!(!rendered.contains("diff -- x"));
            assert!(!rendered.contains("--- a/x"));
            assert!(!rendered.contains("+++ b/x"));
        }
    }
}

#[cfg(test)]
mod syntax_fill_tests {
    use super::*;
    use crate::diff_render::test_output_diff_style_context;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Paragraph;
    use ratatui::widgets::Widget;

    #[test]
    fn rust_and_csharp_keep_syntax_foregrounds_over_light_and_dark_diff_fills() {
        for background in [(18, 20, 24), (255, 255, 255)] {
            let fg = if background.0 == 255 {
                (30, 30, 30)
            } else {
                (230, 230, 230)
            };
            crate::terminal_palette::with_test_default_colors(
                crate::terminal_probe::DefaultColors { fg, bg: background },
                || {
                    let context = test_output_diff_style_context(background, true);
                    for (title, old, new) in [
                        ("code.rs", "let value = 1;", "let value = 2;"),
                        (
                            "Code.cs",
                            "public string Name = \"old\";",
                            "public string Name = \"new\";",
                        ),
                    ] {
                        let source = format!("diff -- {title}\n@@ -1 +1 @@\n-{old}\n+{new}\n");
                        let file = FileDiff {
                            id: "test".into(),
                            title: title.into(),
                            source: source.clone(),
                        };
                        for width in [60, 120] {
                            let text = render_with_style_context(&file, width, context);
                            for kind in [DiffLineType::Insert, DiffLineType::Delete] {
                                let fill = output_diff_line_style(kind, context).bg;
                                assert!(fill.is_some());
                                let colors = text
                                    .lines
                                    .iter()
                                    .flat_map(|line| &line.spans)
                                    .filter(|span| {
                                        span.style.bg == fill
                                            && span.content.chars().any(char::is_alphabetic)
                                    })
                                    .filter_map(|span| span.style.fg)
                                    .collect::<Vec<_>>();
                                assert!(
                                    colors.iter().any(|color| colors.first() != Some(color)),
                                    "syntax colors flattened for {title}, width {width}"
                                );
                            }
                        }
                        assert_eq!(file.source, source);
                    }
                },
            );
        }
    }

    #[test]
    fn blank_changes_fill_each_pane_but_not_the_center_or_missing_rows() {
        let background = (18, 20, 24);
        let context = test_output_diff_style_context(background, true);
        let file = FileDiff {
            id: "blank".into(),
            title: "blank.unknown".into(),
            source: "diff -- blank.unknown\n@@ -1 +1 @@\n-\n+\n".into(),
        };
        let text = render_with_style_context(&file, 120, context);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 8));
        Paragraph::new(text).render(buffer.area, &mut buffer);
        let del = output_diff_line_style(DiffLineType::Delete, context)
            .bg
            .unwrap();
        let add = output_diff_line_style(DiffLineType::Insert, context)
            .bg
            .unwrap();
        assert_eq!(buffer[(0, 3)].bg, del);
        assert_eq!(buffer[(57, 3)].bg, del);
        assert_eq!(buffer[(58, 3)].bg, Color::Reset);
        assert_eq!(buffer[(60, 3)].bg, Color::Reset);
        assert_eq!(buffer[(61, 3)].bg, add);
        assert_eq!(buffer[(118, 3)].bg, add);

        let file = FileDiff {
            source: format!(
                "diff -- blank.unknown\n@@ -1 +1 @@\n-x\n+{}\n",
                "y".repeat(110)
            ),
            ..file
        };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 8));
        Paragraph::new(render_with_style_context(&file, 120, context))
            .render(buffer.area, &mut buffer);
        assert_eq!(buffer[(0, 4)].bg, Color::Reset);
        assert_eq!(buffer[(57, 4)].bg, Color::Reset);
        assert_eq!(buffer[(61, 4)].bg, add);
        assert_eq!(buffer[(118, 4)].bg, add);
    }

    #[test]
    fn syntax_span_boundaries_do_not_split_combining_characters_when_wrapping() {
        let context = test_output_diff_style_context((18, 20, 24), true);
        let syntax = [
            Span::styled("a", Style::default().fg(Color::Blue)),
            Span::styled("\u{301}b", Style::default().fg(Color::Cyan)),
        ];
        let rows = styled_wrap("a\u{301}b", Some(&syntax), 1, DiffLineType::Insert, context);
        let text = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(text, vec!["a\u{301}", "b"]);
        let fill = output_diff_line_style(DiffLineType::Insert, context).bg;
        assert!(rows.iter().flatten().all(|span| span.style.bg == fill));
    }
}
