use super::*;
use pretty_assertions::assert_eq;

#[test]
fn rewrites_canonical_followup() {
    assert_eq!(
        rewrite_followup_line(
            "  - :codex-followup[Review changes]{prompt=\"Show the exact changes.\"}",
        ),
        Some(RewrittenFollowupLine {
            visible_line: "  - Review changes".to_string(),
            followup: Some(FollowupDirective {
                label: "Review changes".to_string(),
                prompt: "Show the exact changes.".to_string(),
            }),
        })
    );
}

#[test]
fn rewrites_observed_bare_prompt_followup() {
    assert_eq!(
        rewrite_followup_line(
            ":codex-followup[Start acceptance run]{Make the harness event-driven.}",
        ),
        Some(RewrittenFollowupLine {
            visible_line: "- Start acceptance run".to_string(),
            followup: Some(FollowupDirective {
                label: "Start acceptance run".to_string(),
                prompt: "Make the harness event-driven.".to_string(),
            }),
        })
    );
}

#[test]
fn accepts_equals_signs_in_bare_prompt_text() {
    assert_eq!(
        rewrite_followup_line(":codex-followup[Update config]{Set retry_count=3.}"),
        Some(RewrittenFollowupLine {
            visible_line: "- Update config".to_string(),
            followup: Some(FollowupDirective {
                label: "Update config".to_string(),
                prompt: "Set retry_count=3.".to_string(),
            }),
        })
    );
}

#[test]
fn preserves_readable_label_when_payload_is_invalid() {
    assert_eq!(
        rewrite_followup_line(":codex-followup[Review changes]{prompt=}"),
        Some(RewrittenFollowupLine {
            visible_line: "- Review changes".to_string(),
            followup: None,
        })
    );
}

#[test]
fn unescapes_quotes_in_canonical_prompt() {
    let rewritten = rewrite_followup_line(
        r#"- :codex-followup[Explain it]{prompt="Explain \"this\" behavior."}"#,
    )
    .expect("follow-up");
    assert_eq!(
        rewritten.followup.expect("action").prompt,
        r#"Explain "this" behavior."#
    );
}
