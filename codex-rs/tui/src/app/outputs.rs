//! Opens the read-only output browser without changing the displayed thread.
use super::*;

impl App {
    pub(super) fn record_output_branch_creations(
        &self,
        server: &AppServerSession,
        completed: &codex_app_server_protocol::ItemCompletedNotification,
    ) {
        let mut creations = crate::branch_association::creations_from_item(&completed.item);
        if creations.is_empty() {
            return;
        }
        let Ok(thread_id) = ThreadId::from_string(&completed.thread_id) else {
            return;
        };
        let Some(runner) = self.workspace_command_runner.clone() else {
            return;
        };
        for creation in &mut creations {
            creation.completed_at_ms = Some(completed.completed_at_ms);
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
        tokio::spawn(async move {
            let Ok(thread) =
                crate::outputs::data::load_workspace_metadata(&handle, &thread_id.to_string())
                    .await
            else {
                return;
            };
            if thread.id != thread_id.to_string()
                || !thread.environments.as_ref().is_some_and(|environments| {
                    !environments.is_empty()
                        && environments.iter().all(|environment| {
                            local_environments.contains(&environment.environment_id)
                        })
                })
            {
                return;
            }
            let associations = crate::branch_association::verify_creations(runner, creations).await;
            if associations.is_empty() {
                return;
            }
            let saved =
                crate::branch_association::persist_associations(path.to_path_buf(), associations)
                    .await;
            match saved {
                Ok(()) => tx.send(AppEvent::OutputBranchesRecorded { thread_id }),
                Err(error) => {
                    tracing::warn!(%thread_id, %error, "failed to save chat branch associations")
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
        view.set_local_plans(self.outputs_local_plans());
        view.set_turn_patch(self.chat_widget.output_turn_patch());
        tui.enter_alt_screen()?;
        self.cancel_pending_key_chord();
        self.overlay = Some(Overlay::Outputs(Box::new(view)));
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    fn outputs_local_plans(&self) -> Vec<crate::outputs::Document> {
        self.transcript_cells
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, cell)| {
                let title = if cell.as_any().is::<crate::history_cell::PlanUpdateCell>() {
                    "Plan progress"
                } else if cell.as_any().is::<crate::history_cell::ProposedPlanCell>() {
                    "Proposed plan"
                } else {
                    return None;
                };
                let markdown = cell.copy_source().map(str::to_owned).unwrap_or_else(|| {
                    cell.raw_lines()
                        .iter()
                        .map(std::string::ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n")
                });
                Some(crate::outputs::Document {
                    id: format!("retained-plan-{index}"),
                    title: title.to_owned(),
                    markdown,
                })
            })
            .take(10)
            .collect()
    }

    pub(super) fn sync_outputs_local_plans(&mut self) {
        if matches!(self.overlay, Some(Overlay::Outputs(_))) {
            let plans = self.outputs_local_plans();
            if let Some(Overlay::Outputs(view)) = &mut self.overlay {
                view.set_local_plans(plans);
                view.set_turn_patch(self.chat_widget.output_turn_patch());
            }
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
