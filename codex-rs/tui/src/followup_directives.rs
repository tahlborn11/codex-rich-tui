//! Parse assistant-authored follow-up suggestions.

use crate::assistant_directives::QuoteEscaping;
use crate::assistant_directives::parse_assistant_directive;

const DIRECTIVE_PREFIX: &str = ":codex-followup[";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FollowupDirective {
    pub(crate) label: String,
    pub(crate) prompt: String,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct RewrittenFollowupLine {
    pub(crate) visible_line: String,
    pub(crate) followup: Option<FollowupDirective>,
}

/// Replace a follow-up directive line with a readable Markdown bullet.
///
/// The canonical form stores the submitted text in a quoted `prompt` attribute. Some model
/// responses omit that attribute and put the prompt directly inside the braces, so accept that
/// emitted form as well. A recognized label is still rendered cleanly when its payload is invalid.
pub(crate) fn rewrite_followup_line(line: &str) -> Option<RewrittenFollowupLine> {
    let content = line.trim_start_matches([' ', '\t']);
    let indent = &line[..line.len() - content.len()];
    let content = content.strip_prefix("- ").unwrap_or(content);
    let remainder = content.strip_prefix(DIRECTIVE_PREFIX)?;
    let label_end = remainder.find(']')?;
    let label = remainder[..label_end].trim();
    if label.is_empty() {
        return None;
    }

    let payload = &remainder[label_end + 1..];
    let prompt = parse_prompt_payload(payload);
    Some(RewrittenFollowupLine {
        visible_line: format!("{indent}- {label}"),
        followup: prompt.map(|prompt| FollowupDirective {
            label: label.to_string(),
            prompt,
        }),
    })
}

fn parse_prompt_payload(payload: &str) -> Option<String> {
    if !payload.starts_with('{') || !payload.ends_with('}') {
        return None;
    }

    let synthetic = format!(":codex-followup{payload}");
    if let Some(directive) = parse_assistant_directive(&synthetic, QuoteEscaping::Backslash)
        && directive.raw.len() == synthetic.len()
        && let Some(prompt) = directive.attributes.get("prompt")
    {
        return nonempty(prompt);
    }

    let bare_prompt = payload[1..payload.len() - 1].trim();
    if bare_prompt.starts_with("prompt=") {
        None
    } else {
        nonempty(bare_prompt)
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
#[path = "followup_directives_tests.rs"]
mod tests;
