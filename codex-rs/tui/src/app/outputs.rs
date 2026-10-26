//! Opens the read-only output browser without changing the displayed thread.
use super::*;

#[path = "outputs_branch_observer.rs"]
mod observers;
pub(super) use observers::BranchObservers;

impl App {
    pub(super) fn record_output_branch_usage(
        &self,
        server: &AppServerSession,
        thread: &str,
        item: &codex_app_server_protocol::ThreadItem,
        observed_at_ms: i64,
    ) {
        let mut creations = crate::branch_association::creations_from_item(item);
        let mut uses = crate::branch_association::uses_from_item(item);
        if creations.is_empty() && uses.is_empty() {
            return;
        }
        let Ok(thread_id) = ThreadId::from_string(thread) else {
            return;
        };
        let Some(runner) = self.workspace_command_runner.clone() else {
            return;
        };
        for creation in &mut creations {
            creation.completed_at_ms = Some(observed_at_ms);
        }
        for usage in &mut uses {
            usage.completed_at_ms = Some(observed_at_ms);
        }
        let local_environments = self
            .environment_manager
            .default_environment_ids()
            .into_iter()
            .filter(|id| {
                self.environment_manager
                    .get_environment(id)
                    .is_some_and(|environment| !environment.is_remote())
            })
            .collect::<Vec<_>>();
        let handle = server.request_handle();
        let path = self
            .config
            .codex_home
            .join("outputs")
            .join(format!("{thread_id}.json"));
        let tx = self.app_event_tx.clone();
        let scope = format!(
            "{thread_id}|{:?}|{:?}|{:?}",
            Arc::as_ptr(&runner),
            self.app_server_target,
            local_environments
        );
        let Some(slot) = self.output_branch_observers.enqueue(scope, uses, creations) else {
            return;
        };
        let permits = self.output_branch_observers.permits.clone();
        tokio::spawn(async move {
            let Ok(_permit) = permits.acquire_owned().await else {
                return;
            };
            let cached = slot
                .lock()
                .ok()
                .and_then(|pending| pending.verified_cwd.clone());
            let cwd = if let Some(cwd) = cached {
                cwd
            } else {
                let metadata =
                    crate::outputs::data::load_workspace_metadata(&handle, &thread_id.to_string())
                        .await;
                let cwd = metadata
                    .ok()
                    .filter(|thread| {
                        thread.id == thread_id.to_string()
                            && thread.environments.as_ref().is_some_and(|environments| {
                                !environments.is_empty()
                                    && environments.iter().all(|environment| {
                                        local_environments.contains(&environment.environment_id)
                                    })
                            })
                    })
                    .map(|thread| thread.cwd.to_path_buf());
                let Some(cwd) = cwd else {
                    if let Ok(mut pending) = slot.lock() {
                        pending.cancel();
                    }
                    return;
                };
                if let Ok(mut pending) = slot.lock() {
                    pending.verified_cwd = Some(cwd.clone());
                }
                cwd
            };
            loop {
                let batch = slot
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.take_batch());
                let Some((mut uses, creations)) = batch else {
                    break;
                };
                for usage in &mut uses {
                    if !usage.cwd.is_absolute() {
                        usage.cwd = cwd.join(&usage.cwd);
                    }
                }
                let mut associations =
                    crate::branch_association::verify_creations(runner.clone(), creations).await;
                associations
                    .extend(crate::branch_association::verify_uses(runner.clone(), uses).await);
                if associations.is_empty() {
                    continue;
                }
                match crate::branch_association::persist_observations(
                    path.to_path_buf(),
                    associations,
                )
                .await
                {
                    Ok(true) => tx.send(AppEvent::OutputBranchesRecorded { thread_id }),
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(%thread_id, %error, "failed to save chat branch associations")
                    }
                }
            }
        });
    }

    fn outputs_scope_key(&self) -> String {
        let runner = self
            .workspace_command_runner
            .as_ref()
            .map(|runner| Arc::as_ptr(runner) as *const () as usize);
        format!(
            "{:?}|{:?}|{:?}|{runner:?}|{:?}",
            self.current_displayed_thread_id(),
            self.chat_widget.config_ref().cwd,
            self.app_server_target,
            self.environment_manager.default_environment_ids()
        )
    }

    pub(super) fn open_outputs(
        &mut self,
        tui: &mut tui::Tui,
        server: &AppServerSession,
    ) -> Result<()> {
        if self.reconnect.offline || self.chat_widget.has_active_modal() {
            self.chat_widget.add_info_message(
                "Outputs is unavailable while reconnecting or answering a request.".to_string(),
                None,
            );
            return Ok(());
        }
        let Some(thread_id) = self.current_displayed_thread_id() else {
            self.chat_widget.add_info_message(
                "Start a conversation to view its outputs.".to_string(),
                None,
            );
            return Ok(());
        };
        if self.overlay.is_some() {
            self.close_transcript_overlay(tui);
        }
        let mut view = crate::outputs::OutputsView::new(
            thread_id.to_string(),
            self.chat_widget.config_ref().cwd.to_path_buf(),
            self.workspace_command_runner.clone(),
            server.request_handle(),
            self.keymap.outputs.clone(),
            self.keymap.pager.clone(),
            tui.frame_requester(),
            Some(
                self.config
                    .codex_home
                    .join("outputs")
                    .join(format!("{thread_id}.json"))
                    .to_path_buf(),
            ),
        );
        view.scope_key = self.outputs_scope_key();
        view.set_supported_environments(
            self.environment_manager
                .default_environment_ids()
                .into_iter()
                .filter(|id| {
                    self.environment_manager
                        .get_environment(id)
                        .is_some_and(|environment| !environment.is_remote())
                })
                .collect(),
        );
        view.set_turn_patch(self.chat_widget.output_turn_patch());
        tui.enter_alt_screen()?;
        self.cancel_pending_key_chord();
        self.overlay = Some(Overlay::Outputs(Box::new(view)));
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    pub(super) fn sync_outputs_turn_patch(&mut self) {
        if let Some(Overlay::Outputs(view)) = &mut self.overlay {
            view.set_turn_patch(self.chat_widget.output_turn_patch());
        }
    }

    pub(super) fn cancel_outputs(&mut self) {
        if let Some(Overlay::Outputs(view)) = self.overlay.as_mut() {
            view.cancel_loads();
            view.is_done = true;
        }
    }

    /// Closing consumes one event so a newly exposed modal cannot receive viewer input.
    pub(super) fn invalidate_outputs_if_needed(&mut self, tui: &mut tui::Tui) -> bool {
        let invalid = match &self.overlay {
            Some(Overlay::Outputs(view)) => {
                view.is_done
                    || self.reconnect.offline
                    || self.chat_widget.has_active_modal()
                    || view.scope_key != self.outputs_scope_key()
            }
            _ => false,
        };
        if invalid {
            self.close_transcript_overlay(tui);
            self.cancel_pending_key_chord();
            tui.frame_requester().schedule_frame();
        }
        invalid
    }
}
