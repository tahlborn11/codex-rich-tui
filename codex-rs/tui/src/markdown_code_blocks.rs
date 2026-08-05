//! Semantic helpers for fenced Markdown code blocks in assistant responses.

use pulldown_cmark::CodeBlockKind;
use pulldown_cmark::Event;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;

pub(crate) fn last_fenced_code_block(markdown: &str) -> Option<String> {
    let mut current = None;
    let mut latest = None;

    for event in Parser::new_ext(markdown, Options::empty()) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => {
                current = Some(String::new());
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)) => {}
            Event::End(TagEnd::CodeBlock) => {
                if let Some(block) = current.take()
                    && !block.is_empty()
                {
                    latest = Some(block);
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(block) = current.as_mut() {
                    block.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(block) = current.as_mut() {
                    block.push('\n');
                }
            }
            Event::Start(_)
            | Event::End(_)
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::Rule
            | Event::FootnoteReference(_)
            | Event::TaskListMarker(_) => {}
        }
    }

    latest
}

#[cfg(test)]
#[path = "markdown_code_blocks_tests.rs"]
mod tests;
