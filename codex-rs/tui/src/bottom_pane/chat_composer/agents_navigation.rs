//! Availability of the empty-prompt agents shortcut and its footer hint.
//!
//! Only local-daemon sessions enable the Left shortcut. Right opens Outputs in all connected modes.
//! Draft contents, transient input surfaces,
//! and explicit editor remaps take precedence over navigation.

use super::*;

impl ChatComposer {
    pub(crate) fn set_agents_navigation_enabled(&mut self, enabled: bool) {
        self.agents_navigation_enabled = enabled;
    }

    pub(crate) fn agents_navigation_key_available(&self) -> bool {
        let move_left = if self.draft.textarea.is_vim_normal_mode() {
            &self.vim_normal_keymap.move_left
        } else {
            &self.editor_keymap.move_left
        };
        move_left.is_pressed(KeyCode::Left.into())
    }

    pub(super) fn agents_navigation_available(&self) -> bool {
        self.agents_navigation_enabled
            && self.agents_navigation_key_available()
            && self.has_focus
            && self.draft.input_enabled
            && self.slash_commands_enabled()
            && self.is_empty()
            && !self.is_in_paste_burst()
            && !self.popup_active()
            && !self.draft.textarea.is_vim_operator_pending()
    }

    pub(super) fn handle_agents_navigation(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Press
            && key_hint::plain(KeyCode::Left).is_press(key)
            && self.agents_navigation_available()
        {
            self.app_event_tx.send(AppEvent::OpenAgentsOverview);
            return true;
        }
        false
    }
}

impl ChatComposer {
    pub(super) fn outputs_navigation_available(&self) -> bool {
        let move_right = if self.draft.textarea.is_vim_normal_mode() {
            &self.vim_normal_keymap.move_right
        } else {
            &self.editor_keymap.move_right
        };
        move_right.is_pressed(KeyCode::Right.into())
            && self.has_focus
            && self.draft.input_enabled
            && self.slash_commands_enabled()
            && self.is_empty()
            && !self.is_in_paste_burst()
            && !self.popup_active()
            && !self.shortcut_overlay_visible()
            && !self.draft.textarea.is_vim_operator_pending()
    }

    pub(super) fn handle_outputs_navigation(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Press
            && key_hint::plain(KeyCode::Right).is_press(key)
            && self.outputs_navigation_available()
        {
            self.app_event_tx.send(AppEvent::OpenOutputs);
            return true;
        }
        false
    }
}
