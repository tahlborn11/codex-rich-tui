use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use codex_app_server_client::AppServerRequestHandle;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

use crate::clipboard_copy::CopyFormat;
use crate::clipboard_copy::CopyStatus;
use crate::key_hint::is_plain_text_key_event;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::keymap::PagerKeymap;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use crate::workspace_command::WorkspaceCommandRunner;

use super::data;
use super::preferences::Preferences;
use super::preferences::RepoBookmark;
use super::tree;
use super::types::Agent;
use super::types::Category;
use super::types::ChangeKind;
use super::types::ConversationData;
use super::types::Document;
use super::types::Repository;

#[path = "repository_tabs.rs"]
mod repository_tabs;
use repository_tabs::RepositoryTabAction;

const NARROW_WIDTH: u16 = 76;
const MAX_DIRECTORY_CACHE_ENTRIES: usize = 20_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    List,
    Preview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Prompt {
    Search,
    Find,
    OpenPath,
}

enum LoadResult {
    Conversation(u64, Result<ConversationData, String>),
    Repositories(u64, Result<Vec<Repository>, String>),
    BranchAssociations(u64, Vec<super::preferences::BranchAssociation>),
    RepositoryScope(u64, Option<String>),
    BranchPruned(u64, Vec<super::preferences::BranchAssociation>),
    BranchAssociationError(u64, String),
    RepositoryChange(u64, String, Result<bool, String>),
    Workspace(u64, Result<Box<codex_app_server_protocol::Thread>, String>),
    Change(u64, Result<String, String>),
    File(u64, Result<String, String>),
    Agent(u64, Result<String, String>),
    Directory(u64, PathBuf, Result<data::DirectoryListing, String>),
}

/// Stateful UI for the full-screen Outputs browser.
///
/// Catalog and detail loads use separate generations. Closing or refreshing
/// cancels the owned tasks; stale responses cannot replace the selected preview.
pub(crate) struct OutputsView {
    pub(crate) thread_id: String,
    pub(crate) cwd: PathBuf,
    /// Filled by the owner from the active thread, workspace and app-server
    /// target. Events from a replaced scope must close this view rather than
    /// let it render data from the old chat.
    pub(crate) scope_key: String,
    runner: Option<WorkspaceCommandRunner>,
    handle: AppServerRequestHandle,
    keymap: ListKeymap,
    frame: FrameRequester,
    preferences_path: Option<PathBuf>,
    preferences: Preferences,
    pub(crate) is_done: bool,
    category: Category,
    focus: Focus,
    selected: usize,
    scroll: usize,
    preview_scroll: usize,
    source_mode: bool,
    prompt: Option<Prompt>,
    query: String,
    open_path: String,
    conversation: ConversationData,
    local_plans: Vec<Document>,
    live_turn_patch: Option<String>,
    repositories: Vec<Repository>,
    all_repositories: Vec<Repository>,
    containing_checkout: Option<String>,
    repositories_loaded: bool,
    repository_load_generation: u64,
    workspace_verified: bool,
    supported_environment_ids: Vec<String>,
    associated_branch: Option<String>,
    directories: BTreeMap<PathBuf, Vec<data::DirectoryEntry>>,
    loading_directories: BTreeSet<PathBuf>,
    expanded_files: BTreeSet<PathBuf>,
    expanded_changes: BTreeSet<PathBuf>,
    repository: usize,
    repository_changes: BTreeMap<String, Result<bool, String>>,
    repository_check_generation: u64,
    repository_check_task: Option<JoinHandle<()>>,
    show_clean_repositories: bool,
    repository_tab_page: usize,
    repository_tab_width: u16,
    reveal_selected_repository: bool,
    repository_tab_hit_areas: Vec<(Rect, RepositoryTabAction)>,
    change_kind: ChangeKind,
    target_ref: Option<String>,
    target_is_explicit: bool,
    base_ref: Option<String>,
    change_text: Option<String>,
    change_files: Vec<super::diff::FileDiff>,
    notice: Option<String>,
    generation: u64,
    detail_generation: u64,
    chord_hint: Option<Vec<(String, String)>>,
    pending_copy: Option<u64>,
    loads: Vec<JoinHandle<()>>,
    load_tx: mpsc::UnboundedSender<LoadResult>,
    load_rx: mpsc::UnboundedReceiver<LoadResult>,
    file_text: Option<String>,
    preview_path: Option<PathBuf>,
    selected_key: Option<String>,
    detail_task: Option<JoinHandle<()>>,
    find_query: String,
    preview_width: u16,
    split_view: bool,
    agent_text: Option<String>,
    tab_hit_areas: Vec<(Rect, Category)>,
    change_tab_hit_areas: Vec<(Rect, ChangeKind)>,
    list_hit_area: Rect,
    last_list_click: Option<(std::time::Instant, u16, u16, u8)>,
    last_clicked_item: Option<String>,
}

impl OutputsView {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        thread_id: String,
        cwd: PathBuf,
        runner: Option<WorkspaceCommandRunner>,
        handle: AppServerRequestHandle,
        keymap: ListKeymap,
        _pager_keymap: PagerKeymap,
        frame: FrameRequester,
        preferences_path: Option<PathBuf>,
    ) -> Self {
        let preferences = preferences_path
            .as_deref()
            .map(Preferences::load_with_associations)
            .unwrap_or_default();
        let (load_tx, load_rx) = mpsc::unbounded_channel();
        let mut view = Self {
            thread_id,
            cwd,
            scope_key: String::new(),
            runner,
            handle,
            keymap,
            frame,
            preferences_path,
            preferences,
            is_done: false,
            category: Category::Plans,
            focus: Focus::List,
            selected: 0,
            scroll: 0,
            preview_scroll: 0,
            source_mode: false,
            prompt: None,
            query: String::new(),
            open_path: String::new(),
            conversation: ConversationData::default(),
            local_plans: Vec::new(),
            live_turn_patch: None,
            repositories: Vec::new(),
            all_repositories: Vec::new(),
            containing_checkout: None,
            repositories_loaded: false,
            repository_load_generation: 0,
            workspace_verified: false,
            supported_environment_ids: Vec::new(),
            associated_branch: None,
            directories: BTreeMap::new(),
            loading_directories: BTreeSet::new(),
            expanded_files: BTreeSet::new(),
            expanded_changes: BTreeSet::new(),
            repository: 0,
            repository_changes: BTreeMap::new(),
            repository_check_generation: 0,
            repository_check_task: None,
            show_clean_repositories: false,
            repository_tab_page: 0,
            repository_tab_width: 80,
            reveal_selected_repository: true,
            repository_tab_hit_areas: Vec::new(),
            change_kind: ChangeKind::Branch,
            target_ref: None,
            target_is_explicit: false,
            base_ref: None,
            change_text: None,
            change_files: Vec::new(),
            notice: None,
            generation: 0,
            detail_generation: 0,
            chord_hint: None,
            pending_copy: None,
            loads: Vec::new(),
            load_tx,
            load_rx,
            file_text: None,
            preview_path: None,
            selected_key: None,
            detail_task: None,
            find_query: String::new(),
            preview_width: 76,
            split_view: true,
            agent_text: None,
            tab_hit_areas: Vec::new(),
            change_tab_hit_areas: Vec::new(),
            list_hit_area: Rect::default(),
            last_list_click: None,
            last_clicked_item: None,
        };
        view.start_initial_loads();
        view
    }

    pub(crate) fn set_turn_patch(&mut self, patch: Option<&str>) {
        if self.live_turn_patch.as_deref() == patch {
            return;
        }
        self.live_turn_patch = patch.map(str::to_owned);
        if self.category == Category::Changes && self.change_kind == ChangeKind::TurnPatch {
            self.start_change_load();
        }
    }

    pub(crate) fn set_supported_environments(&mut self, ids: Vec<String>) {
        self.supported_environment_ids = ids;
    }

    pub(crate) fn refresh_branch_associations(&mut self) {
        if let Some(path) = &self.preferences_path {
            self.preferences.refresh_associations(path);
        }
        self.set_repositories(self.all_repositories.clone());
        if self.workspace_verified {
            self.start_workspace_load();
        }
        if self.category == Category::Changes {
            self.start_change_load();
            self.start_repository_checks();
        }
    }

    pub(crate) fn is_editing(&self) -> bool {
        self.prompt.is_some()
    }
    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn set_chord_hint(&mut self, hint: Option<Vec<(String, String)>>) {
        self.chord_hint = hint;
    }
    pub(crate) fn cancel_loads(&mut self) {
        self.clear_list_click();
        self.stop_repository_checks();
        if let Some(task) = self.detail_task.take() {
            task.abort();
        }
        for task in self.loads.drain(..) {
            task.abort();
        }
        self.generation = self.generation.wrapping_add(1);
        self.repository_load_generation = self.repository_load_generation.wrapping_add(1);
        self.detail_generation = self.detail_generation.wrapping_add(1);
    }
    pub(crate) fn finish_clipboard(
        &mut self,
        completion: &(u64, crate::clipboard_copy::worker::CopyResult),
    ) -> Option<std::time::Duration> {
        if self.pending_copy != Some(completion.0) {
            return None;
        }
        self.pending_copy = None;
        self.notice = Some(match &completion.1 {
            Ok(status) => status.message("preview"),
            Err(error) => format!("Couldn't copy preview: {error}"),
        });
        Some(std::time::Duration::ZERO)
    }

    pub(crate) fn set_conversation(&mut self, data: ConversationData) {
        self.conversation = data;
        self.normalize_file_paths();
        self.conversation.files.sort();
        self.conversation.files.dedup();
        self.merge_local_plans();
        self.clamp_selection();
    }
    /// Seed plans that are still present in the active transcript but have not
    /// reached the app-server history (for example, a live checklist update).
    pub(crate) fn set_local_plans(&mut self, plans: Vec<Document>) {
        self.conversation
            .plans
            .retain(|plan| !plan.id.starts_with("retained-plan-"));
        self.local_plans = plans;
        self.merge_local_plans();
        self.clamp_selection();
    }
    fn normalize_file_paths(&mut self) {
        if self.workspace_verified {
            for path in &mut self.conversation.files {
                if path.is_relative() {
                    *path = self.cwd.join(&*path);
                }
            }
        }
    }
    fn merge_local_plans(&mut self) {
        for plan in &self.local_plans {
            if !self
                .conversation
                .plans
                .iter()
                .any(|existing| existing.markdown == plan.markdown)
            {
                self.conversation.plans.push(plan.clone());
            }
        }
    }
    pub(crate) fn set_repositories(&mut self, repositories: Vec<Repository>) {
        self.all_repositories = repositories;
        self.repositories = self
            .all_repositories
            .iter()
            .filter(|repo| {
                self.chat_is_inside(repo) || self.preferences.association(&repo.identity).is_some()
            })
            .cloned()
            .collect();
        self.repositories_loaded = true;
        let remembered = self.preferences.selected_repo().map(str::to_owned);
        self.repository = match remembered.as_ref() {
            Some(identity) => self.repositories.iter().position(|repo| &repo.identity == identity)
                .unwrap_or_else(|| {
                    self.notice = Some("The remembered checkout is unavailable. Use [ or ] to select an available repository.".into());
                    self.repositories.len()
                }),
            None => self.repository.min(self.repositories.len().saturating_sub(1)),
        };
        self.reset_repo_refs();
        if let Some(repo) = self.selected_repository() {
            self.preferences
                .set_selected_repo(Some(repo.identity.clone()));
            self.save_bookmark();
        }
    }
    fn chat_is_inside(&self, repo: &Repository) -> bool {
        self.containing_checkout.as_deref() == Some(repo.identity.as_str())
            || self.cwd.starts_with(&repo.root)
    }
    pub(crate) fn set_change_text(&mut self, text: Result<String, String>) {
        self.change_files.clear();
        self.change_text = Some(match text {
            Ok(text) if text.trim().is_empty() => "No changes in this view.".into(),
            Ok(text) => {
                self.change_files = super::diff::parse(&text);
                text
            }
            Err(error) => format!("Couldn't load changes: {error}"),
        });
        self.selected_key = None;
        self.preview_scroll = 0;
        self.clamp_selection();
    }
    pub(crate) fn selected_repository(&self) -> Option<&Repository> {
        self.repositories.get(self.repository)
    }
    pub(crate) fn change_request(
        &self,
    ) -> Option<(&Repository, ChangeKind, Option<&str>, Option<&str>)> {
        self.selected_repository().map(|repo| {
            (
                repo,
                self.change_kind,
                self.target_ref.as_deref(),
                self.base_ref.as_deref(),
            )
        })
    }

    pub(crate) fn handle_event(&mut self, tui: &mut Tui, event: TuiEvent) -> io::Result<()> {
        self.drain_loads();
        if matches!(event, TuiEvent::Resize(_) | TuiEvent::Resume) {
            self.clear_list_click();
        }
        if matches!(
            event,
            TuiEvent::Draw | TuiEvent::Resize(_) | TuiEvent::Resume
        ) {
            return self.draw(tui);
        }
        if let TuiEvent::Mouse(mouse) = &event {
            match mouse.kind {
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
                    if self.prompt.is_none() =>
                {
                    if !self.activate_tab(mouse.column, mouse.row) {
                        self.activate_list_row(mouse.column, mouse.row);
                    } else {
                        self.clear_list_click();
                    }
                }
                crossterm::event::MouseEventKind::ScrollDown => {
                    self.scroll_preview_with_mouse(true)
                }
                crossterm::event::MouseEventKind::ScrollUp => self.scroll_preview_with_mouse(false),
                crossterm::event::MouseEventKind::Drag(_)
                | crossterm::event::MouseEventKind::Down(_) => self.clear_list_click(),
                _ => {}
            }
        }
        if let TuiEvent::Paste(text) = &event {
            self.clear_list_click();
            let text = crate::history_cell::sanitize_user_text(text.as_str().into());
            match self.prompt {
                Some(Prompt::OpenPath) => self.open_path.push_str(&text),
                Some(Prompt::Search) => self.query.push_str(&text),
                Some(Prompt::Find) => self.find_query.push_str(&text),
                None => {}
            }
        }
        if let TuiEvent::Key(key) = event
            && key.kind != KeyEventKind::Release
        {
            self.handle_key(tui, key);
        }
        self.draw(tui)
    }

    fn handle_key(&mut self, tui: &mut Tui, key: KeyEvent) {
        self.clear_list_click();
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            self.is_done = true;
            return;
        }
        if self.handle_prompt_key(key) {
            return;
        }
        if key.modifiers == KeyModifiers::ALT {
            match key.code {
                KeyCode::Left => {
                    self.cycle_category(-1);
                    return;
                }
                KeyCode::Right => {
                    self.cycle_category(1);
                    return;
                }
                _ => {}
            }
        }
        let plain = key.kind == KeyEventKind::Press
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT);
        if plain && key.code == KeyCode::Char('/') {
            self.prompt = Some(if self.focus == Focus::Preview {
                Prompt::Find
            } else {
                Prompt::Search
            });
            if self.focus == Focus::Preview {
                self.find_query.clear();
            } else {
                self.query.clear();
            }
            return;
        }
        if plain && key.code == KeyCode::Char('o') && self.category == Category::Files {
            self.prompt = Some(Prompt::OpenPath);
            self.open_path.clear();
            return;
        }
        if plain && key.code == KeyCode::Char('r') {
            self.cancel_loads();
            self.clear_workspace();
            self.selected_key = None;
            self.notice = None;
            self.start_initial_loads();
            return;
        }
        if plain
            && key.code == KeyCode::Char('s')
            && matches!(self.category, Category::Plans | Category::Files)
        {
            self.source_mode = !self.source_mode;
            return;
        }
        if plain && key.code == KeyCode::Char('c') {
            self.copy_preview(tui);
            return;
        }
        if plain && self.category == Category::Changes {
            match key.code {
                KeyCode::Char('a') => {
                    self.show_clean_repositories = !self.show_clean_repositories;
                    self.reveal_selected_repository = true;
                    return;
                }
                KeyCode::Char('[') => {
                    self.cycle_repository(-1);
                    return;
                }
                KeyCode::Char(']') => {
                    self.cycle_repository(1);
                    return;
                }
                KeyCode::Char('v') => {
                    self.cycle_change_kind(1);
                    return;
                }
                KeyCode::Char(c @ '1'..='4') => {
                    self.select_change_kind(ChangeKind::ALL[(c as usize) - ('1' as usize)]);
                    return;
                }
                KeyCode::Char('t') => {
                    self.cycle_target(1);
                    return;
                }
                KeyCode::Char('b') => {
                    self.cycle_base(1);
                    return;
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Tab {
            if self.focus == Focus::List {
                self.load_selected_preview();
            }
            self.focus = match self.focus {
                Focus::List => Focus::Preview,
                Focus::Preview => Focus::List,
            };
            return;
        }
        if key.code == KeyCode::Esc {
            if self.focus == Focus::Preview {
                self.focus = Focus::List;
                return;
            }
            self.is_done = true;
            return;
        }
        let Some(action) = self.keymap.action_for(key) else {
            return;
        };
        match action {
            ListAction::Cancel => self.is_done = true,
            ListAction::Accept => {
                if self.focus == Focus::List && self.toggle_directory() {
                    return;
                }
                self.load_selected_preview();
                self.focus = Focus::Preview;
            }
            ListAction::MoveUp => {
                if self.focus == Focus::List {
                    self.move_selected(-1)
                } else {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1)
                }
            }
            ListAction::MoveDown => {
                if self.focus == Focus::List {
                    self.move_selected(1)
                } else {
                    self.preview_scroll = self.preview_scroll.saturating_add(1)
                }
            }
            ListAction::PageUp => {
                if self.focus == Focus::List {
                    self.move_selected(-10)
                } else {
                    self.preview_scroll = self.preview_scroll.saturating_sub(10)
                }
            }
            ListAction::PageDown => {
                if self.focus == Focus::List {
                    self.move_selected(10)
                } else {
                    self.preview_scroll = self.preview_scroll.saturating_add(10)
                }
            }
            ListAction::JumpTop => {
                if self.focus == Focus::List {
                    self.selected = 0;
                    self.scroll = 0;
                } else {
                    self.preview_scroll = 0;
                }
            }
            ListAction::JumpBottom => {
                if self.focus == Focus::List {
                    self.selected = self.items().len().saturating_sub(1);
                } else {
                    self.preview_scroll = usize::MAX;
                }
            }
            ListAction::MoveLeft => self.move_left(),
            ListAction::MoveRight => self.move_right(),
        }
        if self.focus == Focus::List {
            self.load_selected_preview();
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> bool {
        let Some(prompt) = self.prompt else {
            return false;
        };
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                if prompt == Prompt::Search {
                    self.query.clear();
                    self.selected = 0;
                    self.selected_key = None;
                    self.load_selected_preview();
                }
                true
            }
            KeyCode::Enter => {
                self.prompt = None;
                if prompt == Prompt::OpenPath {
                    let path = PathBuf::from(&self.open_path);
                    let path = if path.is_absolute() {
                        path
                    } else {
                        self.file_root().join(path)
                    };
                    if !self.conversation.files.contains(&path) {
                        self.conversation.files.push(path.clone());
                    }
                    self.query.clear();
                    // Opening an explicit path keeps the explorer selection intact.
                    self.selected_key = None;
                    self.load_path(path);
                    self.focus = Focus::Preview;
                } else if prompt == Prompt::Find {
                    self.find_in_preview();
                } else {
                    self.selected = 0;
                    self.selected_key = None;
                    self.load_selected_preview();
                }
                true
            }
            KeyCode::Backspace => {
                if prompt == Prompt::Search {
                    self.query.pop();
                } else if prompt == Prompt::Find {
                    self.find_query.pop();
                } else {
                    self.open_path.pop();
                }
                true
            }
            _ if is_plain_text_key_event(key) => {
                if let KeyCode::Char(c) = key.code {
                    if prompt == Prompt::Search {
                        self.query.push(c);
                    } else if prompt == Prompt::Find {
                        self.find_query.push(c);
                    } else {
                        self.open_path.push(c);
                    }
                }
                true
            }
            _ => true,
        }
    }

    fn move_selected(&mut self, by: isize) {
        let len = self.items().len();
        if len == 0 {
            return;
        }
        self.selected =
            (self.selected as isize + by).clamp(0, len.saturating_sub(1) as isize) as usize;
    }
    fn move_left(&mut self) {
        if self.focus == Focus::Preview {
            self.focus = Focus::List;
            return;
        }
        if let Some(row) = self.selected_tree_row() {
            if row.is_dir && row.expanded && self.query.is_empty() {
                self.expanded_mut().remove(&row.path);
            } else if let Some(parent) = row.path.parent()
                && let Some(index) = self
                    .tree_rows()
                    .iter()
                    .position(|item| item.is_dir && item.path == parent)
            {
                self.selected = index;
            }
            self.load_selected_preview();
        } else if !self.is_tree_category() {
            self.cycle_category(-1);
        }
    }
    fn move_right(&mut self) {
        if self.focus == Focus::Preview {
            return;
        }
        if let Some(row) = self.selected_tree_row() {
            if row.is_dir {
                if row.expanded {
                    if self.category == Category::Files {
                        self.load_directory(row.path.clone());
                    }
                    if self
                        .tree_rows()
                        .get(self.selected + 1)
                        .is_some_and(|child| child.depth > row.depth)
                    {
                        self.selected += 1;
                    }
                } else {
                    self.toggle_directory();
                }
            } else {
                self.focus = Focus::Preview;
            }
            self.load_selected_preview();
        } else if !self.is_tree_category() {
            self.cycle_category(1);
        }
    }
    fn is_tree_category(&self) -> bool {
        matches!(self.category, Category::Files | Category::Changes)
    }
    fn tree_root(&self) -> PathBuf {
        if self.category == Category::Changes && self.change_kind != ChangeKind::TurnPatch {
            self.selected_repository()
                .map(|repo| repo.root.clone())
                .unwrap_or_else(|| self.cwd.clone())
        } else {
            self.file_root()
        }
    }
    fn tree_rows(&self) -> Vec<tree::TreeRow> {
        let root = self.tree_root();
        let entries = match self.category {
            Category::Files => self
                .directories
                .values()
                .flatten()
                .map(|entry| tree::TreeEntry {
                    id: entry.path.to_string_lossy().into_owned(),
                    path: entry.path.clone(),
                    is_dir: entry.is_dir,
                })
                .collect::<Vec<_>>(),
            Category::Changes => self
                .change_files
                .iter()
                .map(|file| tree::TreeEntry {
                    id: file.id.clone(),
                    path: PathBuf::from(&file.title),
                    is_dir: false,
                })
                .collect(),
            _ => return Vec::new(),
        };
        let expanded = if self.category == Category::Files {
            &self.expanded_files
        } else {
            &self.expanded_changes
        };
        let mut rows = tree::rows(&entries, &root, expanded, &self.query);
        if self.category == Category::Changes {
            let new_files = self
                .change_files
                .iter()
                .filter(|file| file.is_new())
                .map(|file| file.id.as_str())
                .collect::<BTreeSet<_>>();
            for row in &mut rows {
                if !row.is_dir && new_files.contains(row.id.as_str()) {
                    row.label.push_str(" +");
                }
            }
        }
        rows
    }
    fn selected_tree_row(&self) -> Option<tree::TreeRow> {
        self.tree_rows().get(self.selected).cloned()
    }
    fn expanded_mut(&mut self) -> &mut BTreeSet<PathBuf> {
        if self.category == Category::Files {
            &mut self.expanded_files
        } else {
            &mut self.expanded_changes
        }
    }
    fn toggle_directory(&mut self) -> bool {
        let Some(row) = self.selected_tree_row().filter(|row| row.is_dir) else {
            return false;
        };
        if row.expanded && self.query.is_empty() {
            self.expanded_mut().remove(&row.path);
        } else {
            self.expanded_mut().insert(row.path.clone());
            if self.category == Category::Files {
                self.load_directory(row.path);
            }
        }
        self.load_selected_preview();
        true
    }
    fn load_directory(&mut self, directory: PathBuf) {
        if !self.workspace_verified
            || self.directories.contains_key(&directory)
            || self.loading_directories.contains(&directory)
        {
            return;
        }
        let Some(runner) = self.runner.clone() else {
            return;
        };
        if self.directories.len() + self.loading_directories.len() >= 128 {
            self.notice =
                Some("Directory limit reached; refresh or open a narrower folder.".into());
            return;
        }
        if self.directories.values().map(Vec::len).sum::<usize>() >= MAX_DIRECTORY_CACHE_ENTRIES {
            self.notice =
                Some("File entry limit reached; refresh or open a narrower folder.".into());
            return;
        }
        self.loading_directories.insert(directory.clone());
        let generation = self.generation;
        let tx = self.load_tx.clone();
        let root = self.file_root();
        let frame = self.frame.clone();
        self.spawn(async move {
            let result = data::list_directory(runner.as_ref(), &root, &directory).await;
            let _ = tx.send(LoadResult::Directory(generation, directory, result));
            frame.schedule_frame();
        });
    }
    fn scroll_preview_with_mouse(&mut self, down: bool) {
        self.clear_list_click();
        if self.prompt.is_some() {
            return;
        }
        self.load_selected_preview();
        if !self.split_view {
            self.focus = Focus::Preview;
        }
        self.preview_scroll = if down {
            self.preview_scroll.saturating_add(3)
        } else {
            self.preview_scroll.saturating_sub(3)
        };
    }
    fn clear_list_click(&mut self) {
        self.last_list_click = None;
        self.last_clicked_item = None;
    }
    fn activate_list_row(&mut self, x: u16, y: u16) {
        if !self
            .list_hit_area
            .contains(ratatui::layout::Position::new(x, y))
        {
            self.clear_list_click();
            return;
        }
        let selected = self.scroll + usize::from(y.saturating_sub(self.list_hit_area.y));
        let Some((id, _)) = self.items().get(selected).cloned() else {
            self.clear_list_click();
            return;
        };
        self.selected = selected;
        self.focus = Focus::List;
        if self.toggle_directory() {
            self.clear_list_click();
            return;
        }
        self.load_selected_preview();
        if self.last_clicked_item.as_ref() != Some(&id) {
            self.last_list_click = None;
        }
        self.last_clicked_item = Some(id);
        if crate::text_selection::click_count(&mut self.last_list_click, x, y) >= 2 {
            self.focus = Focus::Preview;
        }
    }
    fn cycle_category(&mut self, by: isize) {
        let all = Category::ALL;
        let i = all
            .iter()
            .position(|value| *value == self.category)
            .unwrap_or(0) as isize;
        self.select_category(all[(i + by).rem_euclid(all.len() as isize) as usize]);
    }
    fn activate_tab(&mut self, x: u16, y: u16) -> bool {
        if self.activate_repository_tab(x, y) {
            return true;
        }
        if let Some(kind) = self
            .change_tab_hit_areas
            .iter()
            .find(|(area, _)| area.contains(ratatui::layout::Position::new(x, y)))
            .map(|(_, kind)| *kind)
        {
            if self.change_kind != kind {
                self.select_change_kind(kind);
            }
            self.focus = Focus::List;
            return true;
        }
        let category = self
            .tab_hit_areas
            .iter()
            .find(|(area, _)| area.contains(ratatui::layout::Position::new(x, y)))
            .map(|(_, category)| *category);
        if let Some(category) = category {
            if category != self.category {
                self.select_category(category);
            }
            self.focus = Focus::List;
            return true;
        }
        false
    }
    fn select_category(&mut self, category: Category) {
        self.clear_list_click();
        self.category = category;
        self.focus = Focus::List;
        self.source_mode = false;
        self.selected = 0;
        self.scroll = 0;
        self.query.clear();
        self.selected_key = None;
        self.preview_scroll = 0;
        self.file_text = None;
        self.preview_path = None;
        self.agent_text = None;
        self.detail_generation = self.detail_generation.wrapping_add(1);
        self.start_category_load();
        self.start_repository_checks();
    }
    fn cycle_repository(&mut self, by: isize) {
        let visible = self.visible_repositories();
        if visible.is_empty() {
            return;
        }
        let position = visible
            .iter()
            .position(|index| *index == self.repository)
            .map(|index| index as isize);
        let next = match position {
            Some(position) => (position + by).rem_euclid(visible.len() as isize) as usize,
            None if by < 0 => visible.len() - 1,
            None => 0,
        };
        self.select_repository(visible[next]);
    }
    fn reset_repo_refs(&mut self) {
        if let Some(repo) = self.selected_repository() {
            let (target, base) = self.repository_refs(repo);
            self.target_is_explicit = self.chat_is_inside(repo)
                && self
                    .preferences
                    .bookmark(&self.bookmark_key(&repo.identity))
                    .is_some_and(|bookmark| bookmark.target_is_explicit);
            self.target_ref = target;
            self.base_ref = base;
        } else {
            self.target_ref = None;
            self.target_is_explicit = false;
            self.base_ref = None;
        }
    }
    fn bookmark_key(&self, identity: &str) -> String {
        format!("{}\u{1f}{}", self.thread_id, identity)
    }
    fn save_bookmark(&mut self) {
        let Some(identity) = self.selected_repository().map(|repo| repo.identity.clone()) else {
            return;
        };
        self.preferences.set(
            self.bookmark_key(&identity),
            RepoBookmark {
                target: self.target_ref.clone(),
                base: self.base_ref.clone(),
                target_is_explicit: self.target_is_explicit,
            },
        );
        if let Some(path) = &self.preferences_path
            && let Err(error) = self.preferences.save_bookmarks(path)
        {
            self.notice = Some(format!("Couldn't remember branch selection: {error}"));
        }
    }
    fn cycle_change_kind(&mut self, by: isize) {
        let current = ChangeKind::ALL
            .iter()
            .position(|kind| *kind == self.change_kind)
            .unwrap_or(0) as isize;
        self.select_change_kind(
            ChangeKind::ALL[(current + by).rem_euclid(ChangeKind::ALL.len() as isize) as usize],
        );
    }
    fn select_change_kind(&mut self, kind: ChangeKind) {
        self.focus = Focus::List;
        self.change_kind = kind;
        self.expanded_changes.clear();
        self.query.clear();
        self.selected = 0;
        self.scroll = 0;
        self.selected_key = None;
        self.change_text = None;
        self.change_files.clear();
        self.start_change_load();
        self.start_repository_checks();
    }
    fn cycle_target(&mut self, by: isize) {
        self.cycle_ref(by, true);
    }
    fn cycle_base(&mut self, by: isize) {
        self.cycle_ref(by, false);
    }
    fn cycle_ref(&mut self, by: isize, target: bool) {
        if target
            && self
                .selected_repository()
                .is_some_and(|repo| !self.chat_is_inside(repo))
        {
            self.notice =
                Some("Target follows the branch created by this chat in this repository.".into());
            return;
        }
        let Some(branches) = self.selected_repository().map(|repo| repo.branches.clone()) else {
            return;
        };
        if branches.is_empty() {
            return;
        }
        let current = if target {
            self.target_ref.as_ref()
        } else {
            self.base_ref.as_ref()
        };
        let index = current
            .and_then(|current| {
                branches.iter().position(|branch| {
                    branch == current
                        || Some(branch.as_str()) == current.strip_prefix("refs/heads/")
                })
            })
            .unwrap_or(0) as isize;
        let next = branches[(index + by).rem_euclid(branches.len() as isize) as usize].clone();
        if target {
            self.target_ref = Some(next);
            self.target_is_explicit = true;
        } else {
            self.base_ref = Some(next);
        }
        self.save_bookmark();
        self.change_text = None;
        self.change_files.clear();
        self.detail_generation = self.detail_generation.wrapping_add(1);
        self.start_change_load();
        self.start_repository_checks();
    }
    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.items().len().saturating_sub(1));
    }
    fn spawn(&mut self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        self.loads.push(tokio::spawn(task));
    }
    fn clear_workspace(&mut self) {
        self.workspace_verified = false;
        self.repositories.clear();
        self.all_repositories.clear();
        self.containing_checkout = None;
        self.repository_changes.clear();
        self.repositories_loaded = false;
        self.directories.clear();
        self.loading_directories.clear();
        self.expanded_files.clear();
        self.expanded_changes.clear();
        self.conversation.files.clear();
        self.file_text = None;
        self.change_text = None;
        self.change_files.clear();
        self.selected_key = None;
        self.detail_generation = self.detail_generation.wrapping_add(1);
        if let Some(task) = self.detail_task.take() {
            task.abort();
        }
    }
    fn start_initial_loads(&mut self) {
        let generation = self.generation;
        let tx = self.load_tx.clone();
        let handle = self.handle.clone();
        let thread = self.thread_id.clone();
        let frame = self.frame.clone();
        self.spawn(async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                data::load_conversation(&handle, &thread),
            )
            .await
            .unwrap_or_else(|_| Err("Conversation output request timed out.".into()));
            let _ = tx.send(LoadResult::Conversation(generation, result));
            frame.schedule_frame();
        });
        if self.runner.is_some() {
            let tx = self.load_tx.clone();
            let thread = self.thread_id.clone();
            let handle = self.handle.clone();
            let frame = self.frame.clone();
            self.spawn(async move {
                let result = data::load_workspace_metadata(&handle, &thread)
                    .await
                    .map(Box::new);
                let _ = tx.send(LoadResult::Workspace(generation, result));
                frame.schedule_frame();
            });
        }
    }
    fn start_workspace_load(&mut self) {
        if let Some(runner) = self.runner.clone() {
            self.repository_load_generation = self.repository_load_generation.wrapping_add(1);
            let generation = self.repository_load_generation;
            let tx = self.load_tx.clone();
            let cwd = self.cwd.clone();
            let creations = self.conversation.branch_creations.clone();
            let mut roots = self
                .preferences
                .associations()
                .map(|association| association.repo_root.clone())
                .take(256)
                .collect::<BTreeSet<_>>();
            let recorded = self.preferences.associations().cloned().collect::<Vec<_>>();
            let preferences_path = self.preferences_path.clone();
            let frame = self.frame.clone();
            self.spawn(async move {
                match crate::branch_association::prune_deleted_associations(
                    runner.clone(),
                    preferences_path.clone(),
                    recorded,
                )
                .await
                {
                    Ok(removed) if !removed.is_empty() => {
                        for association in &removed {
                            roots.remove(&association.repo_root);
                        }
                        let _ = tx.send(LoadResult::BranchPruned(generation, removed));
                        frame.schedule_frame();
                    }
                    Err(error) => {
                        let _ = tx.send(LoadResult::BranchAssociationError(generation, error));
                    }
                    _ => {}
                }
                let containing = data::containing_checkout(runner.as_ref(), &cwd).await;
                let Ok(containing) = containing else {
                    let _ = tx.send(LoadResult::Repositories(
                        generation,
                        Err("Couldn't verify repository scope.".into()),
                    ));
                    frame.schedule_frame();
                    return;
                };
                let _ = tx.send(LoadResult::RepositoryScope(generation, containing.clone()));
                let associations =
                    crate::branch_association::verify_creations(runner.clone(), creations).await;
                if !associations.is_empty() {
                    if let Some(path) = preferences_path {
                        if let Err(error) = crate::branch_association::persist_associations(
                            path,
                            associations.clone(),
                        )
                        .await
                        {
                            let _ = tx.send(LoadResult::BranchAssociationError(generation, error));
                        } else {
                            roots.extend(
                                associations
                                    .iter()
                                    .map(|association| association.repo_root.clone()),
                            );
                        }
                    } else {
                        roots.extend(
                            associations
                                .iter()
                                .map(|association| association.repo_root.clone()),
                        );
                    }
                    let _ = tx.send(LoadResult::BranchAssociations(generation, associations));
                }
                let result = if containing.is_some() {
                    data::discover_repositories(runner.as_ref(), &cwd).await
                } else {
                    use futures::StreamExt;
                    let mut repositories = futures::stream::iter(roots.into_iter().take(256))
                        .map(|root| {
                            let runner = runner.clone();
                            async move { data::repository(runner.as_ref(), &root).await }
                        })
                        .buffer_unordered(4)
                        .filter_map(|result| async { result.ok() })
                        .collect::<Vec<_>>()
                        .await;
                    repositories.sort_by(|left, right| left.root.cmp(&right.root));
                    Ok(repositories)
                };
                let _ = tx.send(LoadResult::Repositories(generation, result));
                frame.schedule_frame();
            });
        }
    }
    fn start_category_load(&mut self) {
        if self.category == Category::Changes {
            self.start_change_load();
        } else {
            self.load_selected_preview();
        }
    }
    fn start_change_load(&mut self) {
        self.clear_list_click();
        self.change_files.clear();
        self.selected = 0;
        self.scroll = 0;
        self.selected_key = None;
        self.preview_scroll = 0;
        self.detail_generation = self.detail_generation.wrapping_add(1);
        if let Some(task) = self.detail_task.take() {
            task.abort();
        }
        if self.change_kind == ChangeKind::TurnPatch {
            let patch = self.live_turn_patch.clone().or(self.conversation.turn_patch.clone()).filter(|patch| !patch.is_empty()).unwrap_or_else(|| "No patch recorded for the latest turn. Turn patches cover tool-recorded edits across this chat, not every workspace change.".into());
            self.set_change_text(Ok(patch));
            return;
        }
        self.change_text = Some("Loading changes…".into());
        if !self.workspace_verified {
            self.change_text = Some(
                "Workspace context is not verified. Plans and Agents remain available.".into(),
            );
            return;
        }
        let Some(runner) = self.runner.clone() else {
            self.change_text =
                Some("Workspace commands are unavailable for this connection.".into());
            return;
        };
        let Some((repo, kind, target, base)) = self
            .change_request()
            .map(|(r, k, t, b)| (r.clone(), k, t.map(str::to_owned), b.map(str::to_owned)))
        else {
            self.change_text =
                Some("No associated repository selected. Parent-directory chats show only branches created by this chat. New associations are recorded after successful branch-creation commands.".into());
            return;
        };
        if kind == ChangeKind::Branch && (target.is_none() || base.is_none()) {
            self.change_text = Some("Choose a target and base branch with t and b. No default comparison is available in this repository.".into());
            return;
        }
        let generation = self.detail_generation;
        let tx = self.load_tx.clone();
        let frame = self.frame.clone();
        self.spawn_detail(async move {
            let result = data::load_change(
                runner.as_ref(),
                &repo,
                kind,
                target.as_deref().unwrap_or("HEAD"),
                base.as_deref().unwrap_or("HEAD"),
            )
            .await;
            let _ = tx.send(LoadResult::Change(generation, result));
            frame.schedule_frame();
        });
    }
    fn load_path(&mut self, path: PathBuf) {
        if !self.workspace_verified {
            self.file_text = Some(
                "Workspace context is not verified. Plans and Agents remain available.".into(),
            );
            return;
        }
        let Some(runner) = self.runner.clone() else {
            self.notice = Some("Workspace commands are unavailable for this connection.".into());
            return;
        };
        let root = self.file_root();
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        self.preview_path = Some(path.clone());
        self.file_text = None;
        self.preview_scroll = 0;
        self.detail_generation = self.detail_generation.wrapping_add(1);
        let generation = self.detail_generation;
        let tx = self.load_tx.clone();
        let frame = self.frame.clone();
        self.spawn_detail(async move {
            let result = data::read_file(runner.as_ref(), &path, &root).await;
            let _ = tx.send(LoadResult::File(generation, result));
            frame.schedule_frame();
        });
    }
    fn load_selected_preview(&mut self) {
        let selected = self.items().get(self.selected).map(|item| item.0.clone());
        let key = selected
            .as_ref()
            .map(|id| format!("{:?}:{id}", self.category));
        if key == self.selected_key {
            return;
        }
        self.selected_key = key;
        if self.category == Category::Changes {
            self.preview_scroll = 0;
            return;
        }
        self.detail_generation = self.detail_generation.wrapping_add(1);
        if let Some(task) = self.detail_task.take() {
            task.abort();
        }
        self.preview_scroll = 0;
        self.file_text = None;
        self.agent_text = None;
        match self.category {
            Category::Files => {
                if self.selected_tree_row().is_some_and(|row| row.is_dir) {
                    self.preview_path = None;
                    return;
                }
                if let Some(path) = selected {
                    self.load_path(PathBuf::from(path));
                }
            }
            Category::Agents => {
                if let Some(id) = selected {
                    let generation = self.detail_generation;
                    let tx = self.load_tx.clone();
                    let frame = self.frame.clone();
                    let handle = self.handle.clone();
                    self.spawn_detail(async move {
                        let result = data::load_agent(&handle, &id).await;
                        let _ = tx.send(LoadResult::Agent(generation, result));
                        frame.schedule_frame();
                    });
                }
            }
            Category::Changes => {}
            Category::Plans => {}
        }
    }
    fn file_root(&self) -> PathBuf {
        self.cwd.clone()
    }
    fn spawn_detail(&mut self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        if let Some(previous) = self.detail_task.take() {
            previous.abort();
        }
        self.detail_task = Some(tokio::spawn(task));
    }

    fn drain_loads(&mut self) {
        while let Ok(result) = self.load_rx.try_recv() {
            match result {
                LoadResult::Conversation(generation, result) if generation == self.generation => {
                    match result {
                        Ok(data) => {
                            self.set_conversation(data);
                            if self.workspace_verified {
                                self.start_workspace_load();
                            }
                            self.selected_key = None;
                            self.start_category_load();
                        }
                        Err(error) => {
                            self.notice =
                                Some(format!("Couldn't load conversation outputs: {error}"))
                        }
                    }
                }
                LoadResult::Workspace(generation, result) if generation == self.generation => {
                    match result {
                        Ok(thread) if thread.id == self.thread_id => {
                            self.cwd = thread.cwd.to_path_buf();
                            self.associated_branch = thread.git_info.and_then(|info| info.branch);
                            self.workspace_verified =
                                thread.environments.as_ref().is_some_and(|environments| {
                                    !environments.is_empty()
                                        && environments.iter().all(|environment| {
                                            self.supported_environment_ids
                                                .contains(&environment.environment_id)
                                        })
                                });
                            if self.workspace_verified {
                                self.normalize_file_paths();
                                self.load_directory(self.file_root());
                                self.start_workspace_load();
                            } else {
                                self.notice = Some("Workspace execution context is unavailable or remote; file and Git reads are disabled.".into());
                                self.repositories_loaded = true;
                            }
                        }
                        _ => {
                            self.notice = Some("Couldn't verify this chat's execution context; file and Git reads are disabled.".into());
                            self.repositories_loaded = true;
                        }
                    }
                }
                LoadResult::Repositories(generation, result)
                    if generation == self.repository_load_generation =>
                {
                    match result {
                        Ok(repos) => {
                            if repos.len() >= 256 {
                                self.notice = Some("Showing up to 256 recorded checkouts.".into());
                            }
                            self.set_repositories(repos);
                            self.start_repository_checks();
                            if self.category == Category::Changes {
                                self.start_change_load();
                            }
                        }
                        Err(error) => {
                            self.notice = Some(format!("Couldn't discover repositories: {error}"))
                        }
                    }
                }
                LoadResult::BranchPruned(generation, removed)
                    if generation == self.repository_load_generation =>
                {
                    if let Some(path) = &self.preferences_path {
                        self.preferences.refresh_associations(path);
                    }
                    for association in &removed {
                        let identity = association.repo_root.to_string_lossy();
                        if self
                            .preferences
                            .association(&identity)
                            .is_some_and(|current| current != association)
                        {
                            continue;
                        }
                        self.preferences.remove_association(association);
                        if self.preferences.selected_repo() == Some(identity.as_ref()) {
                            self.preferences.set_selected_repo(None);
                        }
                        let key = self.bookmark_key(&identity);
                        if let Some(mut bookmark) = self.preferences.bookmark(&key).cloned()
                            && bookmark.target.as_deref().is_some_and(|target| {
                                target == association.branch_ref
                                    || Some(target)
                                        == association.branch_ref.strip_prefix("refs/heads/")
                            })
                        {
                            bookmark.target = None;
                            bookmark.target_is_explicit = false;
                            self.preferences.set(key, bookmark);
                        }
                    }
                    if let Some(path) = &self.preferences_path
                        && let Err(error) = self.preferences.save_bookmarks(path)
                    {
                        self.notice =
                            Some(format!("Couldn't clear deleted branch selection: {error}"));
                    }
                    self.refresh_branch_associations();
                }
                LoadResult::BranchAssociations(generation, associations)
                    if generation == self.repository_load_generation =>
                {
                    for association in associations {
                        self.preferences.record_association(association);
                    }
                    if let Some(path) = &self.preferences_path {
                        self.preferences.refresh_associations(path);
                    }
                    self.set_repositories(self.all_repositories.clone());
                    if self.category == Category::Changes {
                        self.start_change_load();
                        self.start_repository_checks();
                    }
                }
                LoadResult::RepositoryScope(generation, containing)
                    if generation == self.repository_load_generation =>
                {
                    self.containing_checkout = containing;
                }
                LoadResult::BranchAssociationError(generation, error)
                    if generation == self.repository_load_generation =>
                {
                    self.notice = Some(format!("Couldn't save chat branch associations: {error}"));
                }
                LoadResult::RepositoryChange(generation, identity, result)
                    if generation == self.repository_check_generation
                        && self.category == Category::Changes
                        && self.change_kind != ChangeKind::TurnPatch =>
                {
                    if self
                        .repositories
                        .iter()
                        .any(|repo| repo.identity == identity)
                    {
                        self.repository_changes.insert(identity, result);
                    }
                }
                LoadResult::Directory(generation, directory, result)
                    if generation == self.generation =>
                {
                    let selected = self.items().get(self.selected).map(|item| item.0.clone());
                    self.loading_directories.remove(&directory);
                    match result {
                        Ok(mut listing) => {
                            let available = MAX_DIRECTORY_CACHE_ENTRIES.saturating_sub(
                                self.directories.values().map(Vec::len).sum::<usize>(),
                            );
                            if listing.entries.len() > available {
                                listing.entries.truncate(available);
                                self.notice = Some(
                                    "File entry limit reached; refresh or open a narrower folder."
                                        .into(),
                                );
                            } else if listing.truncated {
                                self.notice =
                                    Some("Directory listing is partial (2,000 entries).".into());
                            }
                            self.directories.insert(directory, listing.entries);
                            if self.category == Category::Files {
                                if let Some(index) = self
                                    .items()
                                    .iter()
                                    .position(|item| Some(&item.0) == selected.as_ref())
                                {
                                    self.selected = index;
                                }
                                self.clamp_selection();
                                self.load_selected_preview();
                            }
                        }
                        Err(error) => {
                            self.notice = Some(format!("Couldn't browse folder: {error}"))
                        }
                    }
                }
                LoadResult::Change(generation, result) if generation == self.detail_generation => {
                    self.set_change_text(result)
                }
                LoadResult::File(generation, result) if generation == self.detail_generation => {
                    self.file_text =
                        Some(result.unwrap_or_else(|error| format!("Couldn't load file: {error}")))
                }
                LoadResult::Agent(generation, result) if generation == self.detail_generation => {
                    self.agent_text = Some(
                        result
                            .unwrap_or_else(|error| format!("Couldn't load agent output: {error}")),
                    )
                }
                _ => {}
            }
        }
        self.loads.retain(|task| !task.is_finished());
    }
    fn items(&self) -> Vec<(String, String)> {
        let query = self.query.to_lowercase();
        let matches = |text: &str| query.is_empty() || text.to_lowercase().contains(&query);
        match self.category {
            Category::Plans => self
                .conversation
                .plans
                .iter()
                .filter(|d| matches(&d.title))
                .map(|d| (d.id.clone(), d.title.clone()))
                .collect(),
            Category::Files | Category::Changes => self
                .tree_rows()
                .into_iter()
                .map(|row| (row.id, row.label))
                .collect(),
            Category::Agents => self
                .conversation
                .agents
                .iter()
                .filter(|a| matches(&a.title))
                .map(|a| (a.id.clone(), format!("{} · {}", a.title, a.status)))
                .collect(),
        }
    }
    fn preview(&self) -> String {
        let selected = self.items().get(self.selected).map(|item| item.0.clone());
        if self.category == Category::Files
            && self.focus == Focus::Preview
            && self.preview_path.is_some()
        {
            return self
                .file_text
                .clone()
                .unwrap_or_else(|| "Loading file…".into());
        }
        if let Some(row) = self.selected_tree_row().filter(|row| row.is_dir) {
            let relative = row
                .path
                .strip_prefix(self.tree_root())
                .unwrap_or(&row.path)
                .display()
                .to_string();
            return format!(
                "{relative}/\n\nEnter or Right expands the folder; Left collapses it. Select a file to preview it."
            );
        }
        match self.category {
            Category::Plans => self.conversation.plans.iter().find(|d| Some(&d.id) == selected.as_ref()).map(|d| d.markdown.clone()).unwrap_or_else(|| "No plans recorded for this conversation.".into()),
            Category::Files => self.file_text.clone().or_else(|| selected.as_ref().map(|p| format!("{}\n\nLoading…", PathBuf::from(p).strip_prefix(self.file_root()).unwrap_or(std::path::Path::new(p)).display()))).unwrap_or_else(|| "No files in this folder, or the directory is still loading.".into()),
            Category::Changes => self.change_files.iter().find(|file| Some(&file.id) == selected.as_ref()).map(|file| file.source.clone())
                .or_else(|| (!self.change_files.is_empty()).then(|| "No matching changed files.".into()))
                .or_else(|| self.change_text.clone()).unwrap_or_else(|| "Select a repository and refresh to load changes.\n\nBranch changes compare the remembered branch against its base without checking it out. Working changes always belong to the selected checkout.".into()),
            Category::Agents => self.agent_text.clone().or_else(|| self.conversation.agents.iter().find(|a| Some(&a.id) == selected.as_ref()).map(agent_preview)).unwrap_or_else(|| "No subagents are associated with this conversation.".into()),
        }
    }
    fn copy_preview(&mut self, tui: &mut Tui) {
        match tui.copy_transcript_selection(&self.preview(), CopyFormat::PlainText) {
            Ok(status) => {
                if let CopyStatus::Pending(id) = status {
                    self.pending_copy = Some(id);
                }
                self.notice = Some(status.message("preview"));
            }
            Err(error) => self.notice = Some(format!("Couldn't copy preview: {error}")),
        }
    }
    fn draw(&mut self, tui: &mut Tui) -> io::Result<()> {
        tui.draw(u16::MAX, |frame| self.render(frame.area(), frame.buffer))
    }
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let wide = area.width >= NARROW_WIDTH;
        self.split_view = wide;
        let context_height = if self.category == Category::Changes {
            Paragraph::new(
                crate::history_cell::sanitize_user_text(self.context_text().into()).into_owned(),
            )
            .wrap(Wrap { trim: true })
            .line_count(area.width.saturating_sub(2).max(1))
            .clamp(1, 4) as u16
                + 1
        } else {
            2
        };
        let change_tabs_height = if self.category == Category::Changes {
            self.change_tab_layout(area.width)
                .iter()
                .map(|(rect, _)| rect.bottom())
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(change_tabs_height),
                Constraint::Length(self.repository_tabs_height(area.width)),
                Constraint::Length(context_height),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(area);
        self.render_header(rows[0], buf);
        self.change_tab_hit_areas.clear();
        self.repository_tab_hit_areas.clear();
        self.list_hit_area = Rect::default();
        if self.category == Category::Changes {
            self.render_change_tabs(rows[1], buf);
        }
        self.render_repository_tabs(rows[2], buf);
        self.render_context(rows[3], buf);
        if wide {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
                .split(rows[4]);
            self.render_list(cols[0], buf);
            self.render_preview(cols[1], buf);
        } else if self.focus == Focus::List {
            self.render_list(rows[4], buf);
        } else {
            self.render_preview(rows[4], buf);
        }
        self.render_footer(rows[5], buf);
        if let Some(prompt) = self.prompt {
            self.render_prompt(area, buf, prompt);
        }
    }
    fn render_header(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(crate::style::table_separator_style())
            .title(Line::from(vec![
                Span::styled(" OUTPUTS ", crate::style::accent_style()),
                Span::styled(" current chat ", crate::style::secondary_text_style()),
            ]));
        let inner = block.inner(area);
        self.tab_hit_areas.clear();
        let categories = if inner.width < 35 {
            vec![self.category]
        } else {
            Category::ALL.to_vec()
        };
        let mut x = inner.x;
        let tabs = categories
            .into_iter()
            .map(|category| {
                let label = if category == self.category {
                    format!("[ {} ]", category.label())
                } else {
                    format!(" {} ", category.label())
                };
                let width = (label.len() as u16).min(inner.right().saturating_sub(x));
                if width > 0 && inner.height > 0 {
                    self.tab_hit_areas
                        .push((Rect::new(x, inner.y, width, 1), category));
                }
                x = x.saturating_add(width);
                if category == self.category {
                    Span::styled(label, crate::style::selection_style().bold())
                } else {
                    Span::styled(label, crate::style::secondary_text_style())
                }
            })
            .collect::<Vec<_>>();
        Paragraph::new(Line::from(tabs))
            .block(block)
            .render(area, buf);
    }
    fn change_tab_label(kind: ChangeKind) -> &'static str {
        match kind {
            ChangeKind::Branch => "1 Branch",
            ChangeKind::Working => "2 Changes",
            ChangeKind::Staged => "3 Staged",
            ChangeKind::TurnPatch => "4 Turn patch",
        }
    }
    fn change_tab_layout(&self, width: u16) -> Vec<(Rect, ChangeKind)> {
        let prefix = Self::change_tab_prefix_width(width);
        let mut x = prefix;
        let mut y = 0u16;
        ChangeKind::ALL
            .into_iter()
            .map(|kind| {
                let label_width = (Self::change_tab_label(kind).len() as u16 + 2)
                    .min(width.saturating_sub(prefix));
                if x > prefix && x.saturating_add(label_width) > width {
                    x = prefix;
                    y += 1;
                }
                let area = Rect::new(x, y, label_width, 1);
                x = x.saturating_add(label_width);
                (area, kind)
            })
            .collect()
    }
    fn change_tab_prefix_width(width: u16) -> u16 {
        if width >= 24 { 7 } else { 0 }
    }
    fn render_change_tabs(&mut self, area: Rect, buf: &mut Buffer) {
        let prefix = Self::change_tab_prefix_width(area.width);
        if prefix > 0 && area.height > 0 {
            Paragraph::new(Span::styled(
                " VIEW ",
                crate::style::secondary_text_style().bold(),
            ))
            .render(Rect::new(area.x, area.y, prefix, 1), buf);
        }
        for (relative, kind) in self.change_tab_layout(area.width) {
            let rect = Rect::new(
                area.x.saturating_add(relative.x),
                area.y.saturating_add(relative.y),
                relative.width,
                1,
            )
            .intersection(area);
            if rect.is_empty() {
                continue;
            }
            let label = format!(" {} ", Self::change_tab_label(kind));
            let span = if kind == self.change_kind {
                Span::styled(label, crate::style::accent_style().underlined())
            } else {
                Span::styled(label, crate::style::secondary_text_style())
            };
            Paragraph::new(Line::from(span)).render(rect, buf);
            self.change_tab_hit_areas.push((rect, kind));
        }
    }
    fn relative_repo_label(&self, path: &std::path::Path) -> String {
        path.strip_prefix(&self.cwd)
            .ok()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
    }
    fn context_text(&self) -> String {
        if self.category == Category::Changes && self.change_kind == ChangeKind::TurnPatch {
            format!(
                "Recorded tool edits across this chat · {} files",
                self.change_files.len()
            )
        } else if self.category == Category::Changes {
            match self.selected_repository() {
                Some(repo) => {
                    let checkout = repo.branch.as_deref().or(repo.head.as_deref()).unwrap_or("unborn");
                    if self.change_kind == ChangeKind::Branch {
                        let target = self.target_ref.as_deref().unwrap_or("none");
                        let target = target.strip_prefix("refs/heads/").unwrap_or(target);
                        let base = self.base_ref.as_deref().unwrap_or("none");
                        let mismatch = if Some(target) != repo.branch.as_deref() && Some(target) != repo.head.as_deref() {
                            format!("\nCheckout: {checkout} · target differs from checkout")
                        } else { String::new() };
                        format!("Base: {base}  →  Target: {target}{mismatch}")
                    } else {
                        format!("Checkout: {checkout}")
                    }
                }
                None if !self.workspace_verified => self.notice.clone().unwrap_or_else(|| "Workspace context is not verified.".into()),
                None if self.repositories_loaded => "No branches created by this chat were recorded for repositories in this directory. Repos appear after a successful branch-creation command.".into(),
                None => self.notice.clone().unwrap_or_else(|| "Discovering repositories…".into()),
            }
        } else if self.category == Category::Files {
            self.notice.clone().unwrap_or_else(|| {
                format!(
                    "{} · Enter expand/open · ←/→ folders · Alt+←/→ screens",
                    self.relative_repo_label(&self.file_root())
                )
            })
        } else {
            self.notice
                .clone()
                .or_else(|| self.conversation.notice.clone())
                .unwrap_or_else(|| self.cwd.display().to_string())
        }
    }
    fn render_context(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(
            crate::history_cell::sanitize_user_text(self.context_text().into()).into_owned(),
        )
        .style(crate::style::secondary_text_style())
        .wrap(Wrap { trim: true })
        .block(if self.category == Category::Changes {
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(crate::style::table_separator_style())
                .padding(ratatui::widgets::Padding::horizontal(1))
        } else {
            Block::default()
        })
        .render(area, buf);
    }
    fn render_list(&mut self, area: Rect, buf: &mut Buffer) {
        let items = self.items();
        let block =
            Block::default()
                .borders(Borders::RIGHT)
                .title(if self.category == Category::Changes {
                    "Changed files"
                } else {
                    self.category.label()
                });
        let block = block
            .border_style(crate::style::table_separator_style())
            .title_style(crate::style::secondary_text_style().bold());
        self.list_hit_area = block.inner(area);
        self.clamp_selection();
        let height = usize::from(area.height.saturating_sub(1)).max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
        let lines = items
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(usize::from(self.list_hit_area.height))
            .map(|(i, (id, label))| {
                let label = crate::history_cell::sanitize_user_text(label.as_str().into())
                    .replace(['\n', '\r', '\t'], " ");
                let marker = if i == self.selected { "› " } else { "  " };
                let is_new = self.category == Category::Changes
                    && self
                        .change_files
                        .iter()
                        .any(|file| file.id == *id && file.is_new());
                let line = Line::from(format!("{marker}{label}"));
                if is_new {
                    let line = line.green();
                    if i == self.selected && self.focus == Focus::List {
                        line.bold().underlined()
                    } else {
                        line
                    }
                } else if i == self.selected && self.focus == Focus::List {
                    line.reversed()
                } else {
                    line
                }
            })
            .collect::<Vec<_>>();
        Paragraph::new(lines).block(block).render(area, buf);
    }
    fn render_preview(&mut self, area: Rect, buf: &mut Buffer) {
        let hint = if self.source_mode {
            " source "
        } else {
            " c copies source "
        };
        let block = Block::default().title(Line::from(vec![
            Span::styled(" Preview ", crate::style::secondary_text_style().bold()),
            Span::styled(hint, crate::style::secondary_text_style()),
        ]));
        let inner = block.inner(area);
        let width = inner.width.max(1);
        self.preview_width = width;
        let content = self.preview();
        let mut text = self.preview_text(&content, width);
        // Only exceptionally long individual lines need materializing into rows.
        // Normal documents retain Paragraph's existing wrapping behavior.
        if text.lines.iter().any(|line| {
            Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(width)
                > usize::from(u16::MAX)
        }) {
            text.lines = crate::wrapping::word_wrap_lines(text.lines, usize::from(width));
        }
        let max_scroll = Paragraph::new(text.clone())
            .wrap(Wrap { trim: false })
            .line_count(width)
            .saturating_sub(usize::from(inner.height));
        self.preview_scroll = self.preview_scroll.min(max_scroll);
        let mut remaining = self.preview_scroll;
        let mut skipped = 0;
        for line in &text.lines {
            let rows = Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(width);
            if remaining < rows {
                break;
            }
            remaining -= rows;
            skipped += 1;
        }
        text.lines.drain(..skipped);
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(block)
            .scroll((remaining as u16, 0))
            .render(area, buf);
    }
    fn preview_text(&self, content: &str, width: u16) -> ratatui::text::Text<'static> {
        if self.category == Category::Changes {
            let selected = self.items().get(self.selected).map(|item| item.0.clone());
            if let Some(file) = self
                .change_files
                .iter()
                .find(|file| Some(&file.id) == selected.as_ref())
            {
                return super::diff::render(file, width);
            }
        }
        // Keep source/copy exact, but never send document control codes to the terminal.
        let safe = crate::history_cell::sanitize_user_text(content.into()).into_owned();
        if self.source_mode {
            return ratatui::text::Text::raw(safe);
        }
        let language = if self.category == Category::Changes {
            Some("diff".to_owned())
        } else if self.category == Category::Files {
            self.preview_path
                .as_ref()
                .and_then(|p| p.extension())
                .map(|s| s.to_string_lossy().into_owned())
        } else {
            None
        };
        let markdown = matches!(self.category, Category::Plans | Category::Agents)
            || self.category == Category::Files
                && language
                    .as_deref()
                    .is_some_and(|s| matches!(s, "md" | "mdx" | "markdown"));
        if markdown {
            let lines = crate::markdown_render::render_markdown_lines_with_width_and_cwd(
                &safe,
                Some(usize::from(width)),
                Some(&self.file_root()),
            );
            ratatui::text::Text::from(
                lines
                    .into_iter()
                    .map(|mut line| {
                        // This browser copies whole source documents with c. Hide transcript-only
                        // copy icons rather than paint a control that this viewport cannot activate.
                        if let Some(control) = line
                            .source
                            .as_ref()
                            .and_then(|source| source.control.as_ref())
                        {
                            let mut column = 0usize;
                            for span in &mut line.line.spans {
                                span.content = span
                                    .content
                                    .chars()
                                    .map(|ch| {
                                        let hide = control.columns.contains(&column) && ch == '⎘';
                                        column +=
                                            unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                                        if hide { ' ' } else { ch }
                                    })
                                    .collect::<String>()
                                    .into();
                            }
                        }
                        line.line
                    })
                    .collect::<Vec<_>>(),
            )
        } else if let Some(language) = language {
            ratatui::text::Text::from(crate::render::highlight::highlight_code_to_lines(
                &safe, &language,
            ))
        } else {
            ratatui::text::Text::raw(safe)
        }
    }
    fn find_in_preview(&mut self) {
        if self.find_query.is_empty() {
            return;
        }
        let text = self.preview_text(&self.preview(), self.preview_width.max(1));
        let needle = self.find_query.to_lowercase();
        let mut row = 0usize;
        for line in text.lines {
            let source = line.to_string();
            if let Some(index) = source.to_lowercase().find(&needle) {
                // Map case-folded byte offsets back to the displayed source.
                let mut folded_bytes = 0;
                let mut prefix_end = 0;
                for (offset, ch) in source.char_indices() {
                    if folded_bytes >= index {
                        break;
                    }
                    folded_bytes += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
                    prefix_end = offset + ch.len_utf8();
                }
                prefix_end += source[prefix_end..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8);
                let prefix_rows = Paragraph::new(source[..prefix_end].to_owned())
                    .wrap(Wrap { trim: false })
                    .line_count(self.preview_width.max(1));
                self.preview_scroll = row + prefix_rows.saturating_sub(1);
                self.notice = Some(format!("Found {:?}", self.find_query));
                return;
            }
            row += Paragraph::new(line)
                .wrap(Wrap { trim: false })
                .line_count(self.preview_width.max(1));
        }
        self.notice = Some(format!("No match for {:?}", self.find_query));
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let compact_changes = self.category == Category::Changes && area.width < 100;
        let mut text = if compact_changes {
            "1–4 views · [] repo · ←/→ folders · Tab focus · Esc back".into()
        } else {
            String::from("Tab focus · / search · r refresh · c copy · Esc back")
        };
        if self.is_tree_category() && area.width >= 100 {
            text = format!("←/→ folders · Enter open · Alt+←/→ screens · {text}");
        }
        if self.category == Category::Files {
            if area.width < 100 {
                text = "←/→ folders · Enter open · Tab focus · / filter · Esc back".into();
            } else {
                text.push_str(" · o path · s source");
            }
        }
        if self.category == Category::Changes && !compact_changes {
            text.push_str(" · [/] repo · t target · b base · v view");
        }
        if let Some(notice) = &self.notice {
            text.push_str(" · ");
            text.push_str(notice);
        }
        if self.chord_hint.is_some() {
            text.push_str(" · waiting for key chord");
        }
        Paragraph::new(
            crate::history_cell::sanitize_user_text(text.into())
                .into_owned()
                .dim(),
        )
        .block(Block::default().borders(Borders::TOP))
        .render(area, buf);
    }
    fn render_prompt(&self, area: Rect, buf: &mut Buffer, prompt: Prompt) {
        let value = if prompt == Prompt::Find {
            &self.find_query
        } else if prompt == Prompt::Search {
            &self.query
        } else {
            &self.open_path
        };
        let title = if prompt == Prompt::Find {
            " Find in preview "
        } else if prompt == Prompt::Search {
            " Search outputs "
        } else {
            " Open workspace path "
        };
        let width = area.width.min(70);
        let x = area.x + (area.width.saturating_sub(width) / 2);
        let box_area = Rect::new(
            x,
            area.y.saturating_add(area.height / 2).saturating_sub(1),
            width,
            area.height.min(3),
        );
        Clear.render(box_area, buf);
        Paragraph::new(crate::history_cell::sanitize_user_text(value.as_str().into()).into_owned())
            .block(Block::default().borders(Borders::ALL).title(title))
            .render(box_area, buf);
    }
}

fn agent_preview(agent: &Agent) -> String {
    format!(
        "{}\nStatus: {}\n\n{}",
        agent.title, agent.status, agent.summary
    )
}

impl Drop for OutputsView {
    fn drop(&mut self) {
        self.cancel_loads();
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
