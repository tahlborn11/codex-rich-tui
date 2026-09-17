//! Suggested follow-up selection and composer prefilling.

use super::*;
use crate::followup_directives::FollowupDirective;

const FOLLOWUP_VIEW_ID: &str = "assistant-followups";

impl ChatWidget {
    pub(super) fn show_followup_suggestions(&mut self, followups: Vec<FollowupDirective>) {
        let items = followups
            .into_iter()
            .map(|followup| {
                let prompt = followup.prompt;
                SelectionItem {
                    name: followup.label,
                    description: Some(prompt.clone()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::PrefillComposer {
                            text: prompt.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(FOLLOWUP_VIEW_ID),
            title: Some("Suggested follow-ups".to_string()),
            subtitle: Some("Choose one to edit before sending".to_string()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn prefill_composer(&mut self, text: String) {
        self.bottom_pane
            .set_composer_text(text, Vec::new(), Vec::new());
    }
}
