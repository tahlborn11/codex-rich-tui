//! Repository selection and bounded background availability checks for Changes.
use super::*;
use futures::StreamExt;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RepositoryTabAction {
    Select(String),
    ToggleClean,
    Previous,
    Next,
}

impl OutputsView {
    pub(super) fn stop_repository_checks(&mut self) {
        if let Some(task) = self.repository_check_task.take() {
            task.abort();
        }
        self.repository_check_generation = self.repository_check_generation.wrapping_add(1);
    }

    /// Probes match the comparison remembered for each checkout, rather than
    /// applying the selected repository's branch names to every repository.
    pub(super) fn repository_refs(&self, repo: &Repository) -> (Option<String>, Option<String>) {
        let bookmark = self
            .preferences
            .bookmark(&self.bookmark_key(&repo.identity));
        let target = bookmark
            .and_then(|mark| mark.target.clone())
            .or_else(|| {
                self.cwd
                    .starts_with(&repo.root)
                    .then(|| self.associated_branch.clone())
                    .flatten()
            })
            .or_else(|| repo.branch.clone())
            .or_else(|| repo.head.clone());
        let base = bookmark
            .and_then(|mark| mark.base.clone())
            .or_else(|| repo.default_base.clone());
        (target, base)
    }

    pub(super) fn start_repository_checks(&mut self) {
        self.stop_repository_checks();
        self.repository_changes.clear();
        self.repository_tab_page = 0;
        self.reveal_selected_repository = true;
        if self.category != Category::Changes
            || self.change_kind == ChangeKind::TurnPatch
            || !self.workspace_verified
        {
            return;
        }
        let Some(runner) = self.runner.clone() else {
            return;
        };
        let requests = self
            .repositories
            .iter()
            .map(|repo| {
                let (target, base) = self.repository_refs(repo);
                (repo.clone(), target, base)
            })
            .collect::<Vec<_>>();
        if requests.is_empty() {
            return;
        }
        let generation = self.repository_check_generation;
        let kind = self.change_kind;
        let tx = self.load_tx.clone();
        let frame = self.frame.clone();
        self.repository_check_task = Some(tokio::spawn(async move {
            loop {
                let mut probes = futures::stream::iter(requests.clone())
                    .map(|(repo, target, base)| {
                        let runner = runner.clone();
                        async move {
                            let result = if kind == ChangeKind::Branch
                                && (target.is_none() || base.is_none())
                            {
                                Err("Choose a target and base branch.".into())
                            } else {
                                data::has_changes(
                                    runner.as_ref(),
                                    &repo,
                                    kind,
                                    target.as_deref().unwrap_or("HEAD"),
                                    base.as_deref().unwrap_or("HEAD"),
                                )
                                .await
                            };
                            (repo.identity, result)
                        }
                    })
                    .buffer_unordered(4);
                while let Some((identity, result)) = probes.next().await {
                    if tx
                        .send(LoadResult::RepositoryChange(generation, identity, result))
                        .is_err()
                    {
                        return;
                    }
                    frame.schedule_frame();
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        }));
    }

    pub(super) fn visible_repositories(&self) -> Vec<usize> {
        self.repositories
            .iter()
            .enumerate()
            .filter_map(|(index, repo)| {
                (self.show_clean_repositories
                    || index == self.repository
                    || !matches!(self.repository_changes.get(&repo.identity), Some(Ok(false))))
                .then_some(index)
            })
            .collect()
    }

    pub(super) fn select_repository(&mut self, index: usize) {
        if index >= self.repositories.len() {
            return;
        }
        self.repository = index;
        self.focus = Focus::List;
        self.reveal_selected_repository = true;
        self.expanded_changes.clear();
        self.preferences
            .set_selected_repo(self.selected_repository().map(|repo| repo.identity.clone()));
        if let Some(path) = &self.preferences_path
            && let Err(error) = self.preferences.save(path)
        {
            self.notice = Some(format!("Couldn't remember repository: {error}"));
        }
        self.reset_repo_refs();
        self.query.clear();
        self.start_change_load();
    }

    fn repository_tab_label(&self, index: usize, width: u16) -> String {
        let repo = &self.repositories[index];
        let state = match self.repository_changes.get(&repo.identity) {
            Some(Ok(true)) => " *",
            Some(Ok(false)) => " (clean)",
            Some(Err(_)) => " ?",
            None => "",
        };
        let name =
            crate::history_cell::sanitize_user_text(self.relative_repo_label(&repo.root).into())
                .replace(['\n', '\r', '\t'], " ");
        let label = format!("{name}{state}");
        let label = crate::text_formatting::center_truncate_path(
            &label,
            usize::from(width.saturating_sub(2)).min(38),
        );
        if index == self.repository {
            format!("[{label}]")
        } else {
            format!(" {label} ")
        }
    }

    /// At most two rows of repository names per page, regardless of repo count.
    pub(super) fn repository_tab_pages(&self, width: u16) -> Vec<Vec<(Rect, usize)>> {
        if width == 0 {
            return vec![Vec::new()];
        }
        let mut pages = vec![Vec::new()];
        let mut x = 0u16;
        let mut y = 0u16;
        for index in self.visible_repositories() {
            let label = self.repository_tab_label(index, width);
            let tab_width = (label.width() as u16).min(width);
            if x > 0 && x.saturating_add(tab_width) > width {
                x = 0;
                y += 1;
            }
            if y >= 2 {
                pages.push(Vec::new());
                x = 0;
                y = 0;
            }
            if let Some(page) = pages.last_mut() {
                page.push((Rect::new(x, y, tab_width, 1), index));
            }
            x = x.saturating_add(tab_width);
        }
        pages
    }

    pub(super) fn repository_tabs_height(&self, width: u16) -> u16 {
        if self.category != Category::Changes
            || self.change_kind == ChangeKind::TurnPatch
            || self.repositories.is_empty()
        {
            return 0;
        }
        let pages = self.repository_tab_pages(width.saturating_sub(2));
        2 + pages
            .iter()
            .flatten()
            .map(|(rect, _)| rect.bottom())
            .max()
            .unwrap_or(0)
    }

    pub(super) fn render_repository_tabs(&mut self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(crate::style::table_separator_style())
            .render(area, buf);
        let names_width = area.width.saturating_sub(2);
        self.repository_tab_width = names_width;
        let pages = self.repository_tab_pages(names_width);
        if self.reveal_selected_repository {
            if let Some(page) = pages
                .iter()
                .position(|tabs| tabs.iter().any(|(_, index)| *index == self.repository))
            {
                self.repository_tab_page = page;
            }
            self.reveal_selected_repository = false;
        }
        self.repository_tab_page = self.repository_tab_page.min(pages.len().saturating_sub(1));
        let caption = if area.width >= 76 {
            " REPOSITORIES "
        } else {
            " REPOS "
        };
        let caption_width = (caption.len() as u16).min(area.width);
        Paragraph::new(Span::styled(
            caption,
            crate::style::secondary_text_style().bold(),
        ))
        .render(Rect::new(area.x, area.y, caption_width, 1), buf);
        let mode = if self.show_clean_repositories {
            " All repos "
        } else {
            " Changed repos "
        };
        let toggle = Rect::new(
            area.x + caption_width,
            area.y,
            (mode.len() as u16).min(area.width.saturating_sub(caption_width)),
            1,
        );
        Paragraph::new(Line::from(Span::styled(
            mode,
            crate::style::secondary_text_style().underlined(),
        )))
        .render(toggle, buf);
        self.repository_tab_hit_areas
            .push((toggle, RepositoryTabAction::ToggleClean));
        let hidden = self
            .repositories
            .len()
            .saturating_sub(self.visible_repositories().len());
        let mut x = toggle.right();
        if pages.len() > 1 {
            for (label, action) in [
                (" ‹ ", RepositoryTabAction::Previous),
                (" › ", RepositoryTabAction::Next),
            ] {
                let width = (label.width() as u16).min(area.right().saturating_sub(x));
                let rect = Rect::new(x, area.y, width, 1);
                Paragraph::new(label).render(rect, buf);
                if width > 0 {
                    self.repository_tab_hit_areas.push((rect, action));
                }
                x = x.saturating_add(width);
            }
        }
        let hint = format!(
            " {} hidden · a {}{}",
            hidden,
            if self.show_clean_repositories {
                "hide clean"
            } else {
                "show all"
            },
            if pages.len() > 1 {
                format!(" · page {}/{}", self.repository_tab_page + 1, pages.len())
            } else {
                String::new()
            }
        );
        Paragraph::new(hint.dim())
            .render(Rect::new(x, area.y, area.right().saturating_sub(x), 1), buf);
        for (relative, index) in &pages[self.repository_tab_page] {
            let rect = Rect::new(
                area.x.saturating_add(2).saturating_add(relative.x),
                area.y.saturating_add(relative.y).saturating_add(1),
                relative.width,
                1,
            )
            .intersection(area);
            if rect.is_empty() {
                continue;
            }
            let label = self.repository_tab_label(*index, names_width);
            let span = if *index == self.repository {
                Span::styled(label, crate::style::accent_style())
            } else if matches!(
                self.repository_changes
                    .get(&self.repositories[*index].identity),
                Some(Ok(false))
            ) {
                Span::styled(label, crate::style::secondary_text_style())
            } else {
                Span::from(label)
            };
            Paragraph::new(Line::from(span)).render(rect, buf);
            self.repository_tab_hit_areas.push((
                rect,
                RepositoryTabAction::Select(self.repositories[*index].identity.clone()),
            ));
        }
    }

    pub(super) fn activate_repository_tab(&mut self, x: u16, y: u16) -> bool {
        let action = self
            .repository_tab_hit_areas
            .iter()
            .find(|(area, _)| area.contains(ratatui::layout::Position::new(x, y)))
            .map(|(_, action)| action.clone());
        let Some(action) = action else {
            return false;
        };
        match action {
            RepositoryTabAction::Select(identity) => {
                if let Some(index) = self
                    .repositories
                    .iter()
                    .position(|repo| repo.identity == identity)
                    && index != self.repository
                {
                    self.select_repository(index);
                }
            }
            RepositoryTabAction::ToggleClean => {
                self.show_clean_repositories = !self.show_clean_repositories;
                self.reveal_selected_repository = true;
            }
            RepositoryTabAction::Previous => {
                self.repository_tab_page = self.repository_tab_page.saturating_sub(1);
                self.reveal_selected_repository = false;
            }
            RepositoryTabAction::Next => {
                self.repository_tab_page = self.repository_tab_page.saturating_add(1);
                self.reveal_selected_repository = false;
            }
        }
        true
    }
}
