use super::*;
use pretty_assertions::assert_eq;

const FOLLOWUP_MARKDOWN: &str = r#"Done.

- :codex-followup[Start acceptance run]{prompt="Make the harness event-driven."}
:codex-followup[Review changes]{Show the exact harness changes.}"#;

const FENCED_FOLLOWUP_MARKDOWN: &str = r#"The syntax is:

```text
:codex-followup[Example only]{prompt="Do not activate"}
```"#;

#[tokio::test]
async fn final_answer_shows_followups_and_selection_prefills_composer() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());

    complete_assistant_message(
        &mut chat,
        "assistant-followups",
        FOLLOWUP_MARKDOWN,
        Some(MessagePhase::FinalAnswer),
    );

    insta::assert_snapshot!(render_bottom_popup(&chat, /*width*/ 80), @"
      Suggested follow-ups
      Choose one to edit before sending


    › 1. Start acceptance run  Make the harness event-driven.
      2. Review changes        Show the exact harness changes.

      enter select · esc back
    ");

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let prompt = loop {
        match rx.try_recv().expect("follow-up selection event") {
            AppEvent::PrefillComposer { text } => break text,
            _ => continue,
        }
    };
    assert_eq!(prompt, "Make the harness event-driven.");
    chat.prefill_composer(prompt);
    assert_eq!(
        chat.bottom_pane.composer_text(),
        "Make the harness event-driven."
    );
}

#[tokio::test]
async fn replay_renders_followups_without_reopening_picker() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());

    replay_agent_message(
        &mut chat,
        "assistant-followups",
        FOLLOWUP_MARKDOWN,
        ReplayKind::ResumeInitialMessages,
    );

    let rendered = drain_insert_history(&mut rx)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let rendered = lines_to_single_string(&rendered);
    assert!(rendered.contains("Start acceptance run"));
    assert!(rendered.contains("Review changes"));
    assert!(!rendered.contains("codex-followup"));
    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Suggested follow-ups"));
}

#[tokio::test]
async fn final_answer_keeps_fenced_followup_literal_and_does_not_open_picker() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());

    complete_assistant_message(
        &mut chat,
        "assistant-fenced-followup",
        FENCED_FOLLOWUP_MARKDOWN,
        Some(MessagePhase::FinalAnswer),
    );

    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Suggested follow-ups"));
}

#[tokio::test]
async fn replay_keeps_fenced_followup_literal_without_reopening_picker() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());

    replay_agent_message(
        &mut chat,
        "assistant-fenced-followup",
        FENCED_FOLLOWUP_MARKDOWN,
        ReplayKind::ResumeInitialMessages,
    );

    let rendered = drain_insert_history(&mut rx)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let rendered = lines_to_single_string(&rendered);
    assert!(rendered.contains("codex-followup[Example only]"));
    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Suggested follow-ups"));
}
