//! Regression coverage for inline code controls in an unfocused side pane.

use super::side_companion::SideCompanion;
use super::sidebar::PaneFocus;
use super::sidebar::SidebarContent;
use super::test_support::enable_side_retention;
use super::test_support::make_test_app;
use super::test_support::side_frame;
use super::*;
use crate::history_cell::AgentMarkdownCell;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::layout::Size;
use std::path::Path;
use std::sync::Arc;

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> TuiEvent {
    TuiEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

#[tokio::test]
async fn passive_code_control_copies_without_switching_focus() -> Result<()> {
    let mut app = make_test_app().await;
    enable_side_retention(&mut app)?;
    app.local_settings.tui.animations = false;
    let parent = ThreadId::new();
    let child = ThreadId::new();
    app.active_thread_id = Some(parent);
    app.primary_thread_id = Some(parent);
    app.transcript_cells = vec![Arc::new(AgentMarkdownCell::new(
        "```rust\nlet main = true;\n```".into(),
        Path::new("/"),
    ))];
    app.side_threads.insert(child, SideThreadState::new(parent));
    let mut companion = SideCompanion::empty(app.config.cwd.clone());
    companion.cells = vec![Arc::new(AgentMarkdownCell::new(
        "```rust\nlet passive = true;\n```".into(),
        Path::new("/"),
    ))];
    app.side_companions.insert(child, companion);
    app.sidebars
        .entry(Some(parent))
        .or_default()
        .show(SidebarContent::Chat(child), PaneFocus::Main);

    let size = Size::new(/*width*/ 120, /*height*/ 20);
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.terminal = crate::custom_terminal::Terminal::with_screen_size_and_cursor_position_for_test(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        size,
        ratatui::layout::Position::new(0, 0),
    );
    tui.set_owned_screen(/*owned*/ true)?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    side_frame(&mut app, &mut tui, size)?;
    let panes = app
        .conversation_frame
        .side_pane()
        .expect("side pane should be painted");
    let buffer = tui.terminal.previous_buffer();
    let (column, row) = (panes.passive.top()..panes.passive.bottom())
        .flat_map(|row| {
            (panes.passive.left()..panes.passive.right()).map(move |column| (column, row))
        })
        .find(|(column, row)| buffer[(*column, *row)].symbol() == "⎘")
        .expect("passive code block should show a copy control");
    let (main_column, main_row) = (panes.focused.top()..panes.focused.bottom())
        .flat_map(|row| {
            (panes.focused.left()..panes.focused.right()).map(move |column| (column, row))
        })
        .find(|(column, row)| buffer[(*column, *row)].symbol() == "⎘")
        .expect("focused code block should show a copy control");
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        mouse(MouseEventKind::Moved, main_column, main_row),
    )
    .await?;
    side_frame(&mut app, &mut tui, size)?;
    assert!(
        tui.terminal.previous_buffer()[(main_column, main_row)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    );

    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        mouse(MouseEventKind::Moved, column, row),
    )
    .await?;
    assert_eq!(app.current_displayed_thread_id(), Some(parent));
    side_frame(&mut app, &mut tui, size)?;
    assert!(
        tui.terminal.previous_buffer()[(column, row)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED),
        "hover must remain with the passive control"
    );
    assert!(
        !tui.terminal.previous_buffer()[(main_column, main_row)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED),
        "entering the passive pane must clear the focused control hover"
    );

    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        mouse(MouseEventKind::Moved, 0, 0),
    )
    .await?;
    side_frame(&mut app, &mut tui, size)?;
    assert!(
        !tui.terminal.previous_buffer()[(column, row)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED),
        "leaving the passive pane must clear its control hover"
    );
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        mouse(MouseEventKind::Moved, column, row),
    )
    .await?;

    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.handle_tui_event(&mut tui, &mut app_server, mouse(kind, column, row))
            .await?;
    }
    assert_eq!(app.current_displayed_thread_id(), Some(parent));
    assert!(
        tui.clipboard.is_busy(),
        "the passive control should submit its own copy request"
    );
    let companion = app.side_companions.get_mut(&child).expect("passive pane");
    assert_eq!(
        companion.view.selected_text(&companion.cells),
        None,
        "copying code must not create a transcript selection"
    );

    // The passive pane can be redrawn with different geometry before mouse-up.
    // Cancelling that now-stale ownership must clear the pressed styling.
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        mouse(MouseEventKind::Down(MouseButton::Left), column, row),
    )
    .await?;
    let (owner, mut stale_area) = app
        .passive_link_drag
        .expect("the passive control press should retain gesture ownership");
    stale_area.width = stale_area.width.saturating_sub(1);
    app.passive_link_drag = Some((owner, stale_area));
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        mouse(MouseEventKind::Up(MouseButton::Left), column, row),
    )
    .await?;
    assert_eq!(
        app.passive_link_drag, None,
        "stale ownership must be cancelled"
    );
    side_frame(&mut app, &mut tui, size)?;
    let panes = app
        .conversation_frame
        .side_pane()
        .expect("side pane should be repainted");
    let buffer = tui.terminal.previous_buffer();
    let (column, row) = (panes.passive.top()..panes.passive.bottom())
        .flat_map(|row| {
            (panes.passive.left()..panes.passive.right()).map(move |column| (column, row))
        })
        .find(|(column, row)| buffer[(*column, *row)].symbol() == "⎘")
        .expect("passive code block should retain its copy control");
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        mouse(MouseEventKind::Moved, column, row),
    )
    .await?;
    side_frame(&mut app, &mut tui, size)?;
    let modifier = tui.terminal.previous_buffer()[(column, row)].modifier;
    assert!(modifier.contains(ratatui::style::Modifier::REVERSED));
    assert!(
        !modifier.contains(ratatui::style::Modifier::UNDERLINED),
        "a cancelled passive gesture must not leave the code control pressed"
    );
    app_server.shutdown().await?;
    Ok(())
}
