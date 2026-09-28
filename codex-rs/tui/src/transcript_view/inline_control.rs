//! Mouse interaction for source-free controls embedded in transcript rows.

use super::*;
use crate::terminal_hyperlinks::LineControlAction;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::layout::Position as ScreenPosition;
use ratatui::style::Modifier;
use ratatui::style::Style;
use std::time::Duration;
use std::time::Instant;

const COPY_CONFIRMED_DURATION: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
struct ControlIdentity {
    key: EntryKey,
    row: usize,
}

struct ControlTarget {
    identity: ControlIdentity,
    action: LineControlAction,
}

#[derive(Default)]
pub(super) struct InlineControlState {
    pointer: Option<ScreenPosition>,
    pressed: Option<ControlIdentity>,
    activated: Option<ControlIdentity>,
    pending: Option<(u64, ControlIdentity)>,
    confirmed: Option<(ControlIdentity, Instant)>,
}

impl TranscriptView {
    pub(super) fn render_inline_control(
        &self,
        layout: &TextLayout,
        row: usize,
        area: Rect,
        buf: &mut Buffer,
        key: EntryKey,
    ) {
        let Some(columns) = layout.control_columns(row) else {
            return;
        };
        let target = Rect::new(
            area.x + columns.start,
            area.y,
            columns.end - columns.start,
            /*height*/ 1,
        );
        if self
            .inline_control
            .confirmed
            .as_ref()
            .is_some_and(|(confirmed, expiry)| {
                confirmed == &ControlIdentity { key, row } && *expiry > Instant::now()
            })
        {
            buf[(target.x, target.y)].set_symbol("✓");
        }
        let hovered = self
            .inline_control
            .pointer
            .is_some_and(|point| target.contains(point));
        let pressed = self
            .inline_control
            .pressed
            .as_ref()
            .is_some_and(|pressed| pressed.key == key && pressed.row == row && hovered);
        let mut style = Style::default()
            .fg(crate::style::accent_color())
            .bold()
            .remove_modifier(Modifier::DIM);
        if hovered {
            style = style.reversed();
        }
        if pressed {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        buf.set_style(target, style);
    }

    pub(super) fn handle_inline_control_mouse(&mut self, event: MouseEvent) -> Option<ViewAction> {
        let point = ScreenPosition::new(event.column, event.row);
        let previous = self
            .inline_control
            .pointer
            .and_then(|pointer| self.inline_control_at(pointer));
        let current = self.inline_control_at(point);
        self.inline_control.pointer = Some(point);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left)
                if event.modifiers == KeyModifiers::NONE && current.is_some() =>
            {
                self.inline_control.activated = None;
                self.inline_control.pressed = current.map(|target| target.identity);
                Some(ViewAction::Changed)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.inline_control.pressed = None;
                None
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let pressed = self.inline_control.pressed.take()?;
                let target = current.filter(|target| target.identity == pressed);
                let action = target.as_ref().map(|target| target.action.clone());
                self.inline_control.activated = target.map(|target| target.identity);
                Some(match action {
                    Some(LineControlAction::CopyCode(text)) => ViewAction::CopyCode(text),
                    None => ViewAction::Changed,
                })
            }
            MouseEventKind::Drag(MouseButton::Left) if self.inline_control.pressed.is_some() => {
                Some(ViewAction::Changed)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.inline_control.pressed = None;
                None
            }
            MouseEventKind::Moved
                if previous.as_ref().map(|target| &target.identity)
                    != current.as_ref().map(|target| &target.identity) =>
            {
                Some(ViewAction::Changed)
            }
            _ => None,
        }
    }

    pub(crate) fn show_code_copy_result(
        &mut self,
        result: &Result<crate::clipboard_copy::CopyStatus, String>,
    ) -> Option<Duration> {
        let identity = self.inline_control.activated.take()?;
        self.inline_control
            .confirmed
            .take_if(|(confirmed, _)| confirmed == &identity);
        match result {
            Ok(crate::clipboard_copy::CopyStatus::Confirmed) => {
                self.inline_control.confirmed =
                    Some((identity, Instant::now() + COPY_CONFIRMED_DURATION));
                Some(COPY_CONFIRMED_DURATION)
            }
            Ok(crate::clipboard_copy::CopyStatus::Pending(id)) => {
                self.inline_control.pending = Some((*id, identity));
                None
            }
            Ok(
                crate::clipboard_copy::CopyStatus::Unconfirmed
                | crate::clipboard_copy::CopyStatus::Busy,
            )
            | Err(_) => None,
        }
    }

    pub(crate) fn finish_code_copy(
        &mut self,
        completion: &(u64, crate::clipboard_copy::worker::CopyResult),
        current: bool,
    ) -> Option<Duration> {
        let (_, identity) = self
            .inline_control
            .pending
            .take_if(|(id, _)| *id == completion.0)?;
        if current && completion.1 == Ok(crate::clipboard_copy::CopyStatus::Confirmed) {
            self.inline_control.confirmed =
                Some((identity, Instant::now() + COPY_CONFIRMED_DURATION));
            return Some(COPY_CONFIRMED_DURATION);
        }
        Some(Duration::ZERO)
    }

    fn inline_control_at(&self, point: ScreenPosition) -> Option<ControlTarget> {
        if !self.area.contains(point) {
            return None;
        }
        let visible = self.visible.get(usize::from(point.y - self.area.y))?;
        let columns = visible.layout.control_columns(visible.row)?;
        let area = Rect::new(
            self.area.x + columns.start,
            point.y,
            columns.end - columns.start,
            /*height*/ 1,
        );
        if !area.contains(point) {
            return None;
        }
        let action = visible
            .layout
            .control_at(visible.row, point.x - self.area.x)?;
        Some(ControlTarget {
            identity: ControlIdentity {
                key: visible.key,
                row: visible.row,
            },
            action,
        })
    }
}

#[cfg(test)]
#[path = "inline_control_tests.rs"]
mod tests;
