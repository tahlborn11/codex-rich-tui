//! A conservative fast path for one open, top-level, language-tagged code fence.

use crate::markdown_render::code_panel_supports_full_width;
use crate::markdown_render::hard_wrap_code_line;
use crate::render::highlight::MAX_HIGHLIGHT_LINE_BYTES;
use crate::render::highlight::StreamingCodeHighlighter;
use crate::render::highlight::syntax_theme_revision;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::text::Span;

/// An append-only fence whose original source is identical to pulldown-cmark's code text.
pub(super) struct OpenCodeFence {
    marker: u8,
    marker_len: usize,
    language: String,
    content_start: usize,
    source_len: usize,
    theme_revision: u64,
    panel_width: Option<usize>,
    panel_full_width: bool,
    /// Initialized only if another chunk arrives, avoiding duplicate work on one-shot fences.
    highlighter: Option<StreamingCodeHighlighter>,
}

impl OpenCodeFence {
    /// Recognize the final mutable top-level block after its canonical render.
    ///
    /// `source` is the final block's suffix, while `source_len` includes the entire committed
    /// Markdown source and anchors the retained fence within it.
    ///
    /// Indentation, CR/NUL normalization, and escaped info strings stay on the canonical path.
    /// Any line that could end the fence also stays there, including deliberately conservative
    /// false positives, so this fast path need not duplicate CommonMark's closing-fence grammar.
    pub(super) fn detect(
        source: &str,
        source_len: usize,
        theme_revision: u64,
        panel_width: Option<usize>,
    ) -> Option<Self> {
        let marker = *source.as_bytes().first()?;
        if marker != b'`' && marker != b'~' {
            return None;
        }
        let (opening, code) = source.split_once('\n')?;
        let marker_len = opening.bytes().take_while(|byte| *byte == marker).count();
        if marker_len < 3
            || !source.ends_with('\n')
            || theme_revision != syntax_theme_revision()
            || source.contains(['\r', '\0'])
        {
            return None;
        }
        let info = opening[marker_len..].trim_matches([' ', '\t', '\u{b}', '\u{c}']);
        if info.contains(['&', '\\']) || (marker == b'`' && info.contains('`')) {
            return None;
        }
        let language = info
            .split([',', ' ', '\t'])
            .next()
            .filter(|language| !language.is_empty())?;
        if matches!(language, "md" | "markdown") {
            return None;
        }
        // Retain at most one bounded language token, never the streamed code itself.
        if language.len() > MAX_HIGHLIGHT_LINE_BYTES
            || has_possible_closing_line(code, marker, marker_len)
        {
            return None;
        }
        let panel_full_width =
            panel_width.is_some_and(|width| code_panel_supports_full_width(language, width));
        Some(Self {
            marker,
            marker_len,
            language: language.to_string(),
            content_start: source_len.checked_sub(code.len())?,
            source_len,
            theme_revision,
            panel_width,
            panel_full_width,
            highlighter: None,
        })
    }

    /// Append code only while the caller's newline-committed source extends this same fence.
    ///
    /// Returning `None` consumes the retained state and requires a canonical whole-fence render.
    /// Successful lines include the empty indent span expected by the canonical Markdown writer.
    pub(super) fn append(
        mut self,
        raw_source: &str,
        committed_source: &str,
    ) -> Option<(Self, Vec<HyperlinkLine>)> {
        if self.source_len.checked_add(committed_source.len()) != Some(raw_source.len())
            || !raw_source.ends_with(committed_source)
            || committed_source.contains(['\r', '\0'])
            || has_possible_closing_line(committed_source, self.marker, self.marker_len)
            || self.theme_revision != syntax_theme_revision()
        {
            return None;
        }
        let highlighter = match self.highlighter.take() {
            Some(highlighter) => highlighter,
            None => StreamingCodeHighlighter::new(
                &raw_source[self.content_start..self.source_len],
                &self.language,
                self.theme_revision,
            )?,
        };
        let (highlighter, lines) = highlighter.append(committed_source)?;
        self.highlighter = Some(highlighter);
        self.source_len = raw_source.len();
        let panel_style = crate::style::user_message_style();
        let lines = lines
            .into_iter()
            .flat_map(|line| {
                let inner_width = self
                    .panel_width
                    .map(|width| {
                        width.saturating_sub(
                            /*left rail*/
                            2 + if self.panel_full_width {
                                /*padding + right rail*/
                                2
                            } else {
                                0
                            },
                        )
                    })
                    .unwrap_or(usize::MAX)
                    .max(1);
                hard_wrap_code_line(&line, inner_width)
            })
            .map(|mut line| {
                line.spans
                    .insert(/*index*/ 0, Span::styled("│ ", panel_style.dim()));
                if self.panel_full_width
                    && let Some(width) = self.panel_width
                    && line.width() < width
                {
                    let padding = width.saturating_sub(line.width() + 1);
                    line.push_span(Span::styled(" ".repeat(padding), panel_style));
                    line.push_span(Span::styled("│", panel_style.dim()));
                }
                for span in &mut line.spans {
                    span.style = span.style.patch(panel_style);
                }
                HyperlinkLine::new(line)
            })
            .collect();
        Some((self, lines))
    }
}

/// Conservatively detect any line that might close the fence.
///
/// False positives intentionally fall back to CommonMark instead of duplicating its closer rules.
fn has_possible_closing_line(source: &str, marker: u8, marker_len: usize) -> bool {
    source.lines().any(|line| {
        line.trim_start_matches([' ', '\t'])
            .bytes()
            .take_while(|byte| *byte == marker)
            .count()
            >= marker_len
    })
}

#[cfg(test)]
#[path = "code_fence_tests.rs"]
mod tests;
