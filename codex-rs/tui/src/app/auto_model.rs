use super::App;
use crate::auto_selection;
use crate::auto_selection::AutoSelection;
use codex_protocol::ThreadId;

impl App {
    pub(super) async fn restore_auto_selection_for_thread(&mut self, thread_id: ThreadId) {
        let Some(auto_config) = self.config.tui_auto.as_ref() else {
            self.chat_widget.set_auto_selected(false);
            return;
        };
        let default_selection = if auto_config.default_selected {
            AutoSelection::Auto
        } else {
            AutoSelection::Manual
        };
        let (selection, should_persist) =
            match auto_selection::load(self.config.codex_home.as_path(), thread_id).await {
                Ok(Some(selection)) => (selection, false),
                Ok(None) => (default_selection, true),
                Err(err) => {
                    tracing::warn!(%thread_id, %err, "failed to restore persisted Auto selection");
                    (default_selection, true)
                }
            };
        let auto_selected = selection == AutoSelection::Auto;
        let was_auto_selected = self.chat_widget.auto_selected();
        self.chat_widget.set_auto_selected(auto_selected);
        if auto_selected && was_auto_selected {
            self.chat_widget.warm_auto_classifier();
        }
        if should_persist
            && let Err(err) =
                auto_selection::persist(self.config.codex_home.as_path(), thread_id, selection)
                    .await
        {
            tracing::warn!(%thread_id, %err, "failed to persist initial Auto selection");
        }
    }

    pub(super) async fn persist_active_auto_selection(&self, selection: AutoSelection) {
        if self.config.tui_auto.is_none() {
            return;
        }
        let Some(thread_id) = self.current_displayed_thread_id() else {
            return;
        };
        if let Err(err) =
            auto_selection::persist(self.config.codex_home.as_path(), thread_id, selection).await
        {
            tracing::warn!(%thread_id, %err, "failed to persist Auto selection");
        }
    }
}
