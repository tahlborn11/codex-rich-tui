use super::*;
use crate::history_cell::AgentMarkdownCell;
use crossterm::event::MouseEvent;
use pretty_assertions::assert_eq;

fn transcript(markdown: &str, width: u16) -> (TranscriptView, Vec<Arc<dyn HistoryCell>>, Buffer) {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(AgentMarkdownCell::new(
        markdown.into(),
        std::path::Path::new("/"),
    ))];
    let mut view = TranscriptView::default();
    let area = Rect::new(/*x*/ 2, /*y*/ 1, width, /*height*/ 14);
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer, &cells);
    (view, cells, buffer)
}

fn icons(buffer: &Buffer) -> Vec<ScreenPosition> {
    (buffer.area.top()..buffer.area.bottom())
        .flat_map(|y| {
            (buffer.area.left()..buffer.area.right()).map(move |x| ScreenPosition::new(x, y))
        })
        .filter(|point| buffer[*point].symbol() == "⎘")
        .collect()
}

fn mouse(kind: MouseEventKind, point: ScreenPosition) -> MouseEvent {
    MouseEvent {
        kind,
        column: point.x,
        row: point.y,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn code_controls_copy_their_own_exact_blocks() {
    let markdown = "```rust\nlet one = 1;\n```\n\n```python\nprint('two')\n```";
    let (mut view, cells, buffer) = transcript(markdown, /*width*/ 40);
    let icons = icons(&buffer);
    assert_eq!(
        icons.len(),
        2,
        "{}",
        crate::transcript_view::tests::text(&buffer)
    );

    assert!(matches!(
        view.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), icons[1]),
            &cells
        ),
        Some(ViewAction::Changed)
    ));
    let action = view.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), icons[1]),
        &cells,
    );
    let Some(ViewAction::CopyCode(text)) = action else {
        panic!("code control should copy its own block");
    };
    assert_eq!(text.as_ref(), "print('two')\n");
    assert_eq!(view.selected_text(&cells), None);

    assert_eq!(
        view.show_code_copy_result(&Ok(crate::clipboard_copy::CopyStatus::Confirmed)),
        Some(std::time::Duration::from_secs(5))
    );
    let mut confirmed = Buffer::empty(view.area);
    view.render(view.area, &mut confirmed, &cells);
    assert_eq!(confirmed[icons[1]].symbol(), "✓");
    assert_eq!(confirmed[icons[0]].symbol(), "⎘");
}

#[test]
fn pending_code_copy_shows_check_only_after_confirmed_completion() {
    let (mut view, cells, buffer) = transcript("```text\ncontent\n```", /*width*/ 32);
    let icon = icons(&buffer)[0];
    view.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), icon), &cells);
    assert!(matches!(
        view.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), icon), &cells),
        Some(ViewAction::CopyCode(_))
    ));

    assert_eq!(
        view.show_code_copy_result(&Ok(crate::clipboard_copy::CopyStatus::Pending(7))),
        None
    );
    let mut pending = Buffer::empty(view.area);
    view.render(view.area, &mut pending, &cells);
    assert_eq!(pending[icon].symbol(), "⎘");

    assert_eq!(
        view.finish_code_copy(
            &(7, Ok(crate::clipboard_copy::CopyStatus::Confirmed)),
            /*current*/ true,
        ),
        Some(std::time::Duration::from_secs(5))
    );
    let mut confirmed = Buffer::empty(view.area);
    view.render(view.area, &mut confirmed, &cells);
    assert_eq!(confirmed[icon].symbol(), "✓");
}

#[test]
fn narrow_code_controls_stay_visible_and_copy_the_exact_source() {
    for (width, language) in [(9, "typescript"), (10, "👩‍💻language")] {
        let source = "line one\nline two\n";
        let markdown = format!("```{language}\n{source}```");
        let (mut view, cells, buffer) = transcript(&markdown, width);
        let icon = icons(&buffer)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("copy icon missing at width {width}"));
        assert_eq!(icon.y, view.area.y, "icon must remain on the header row");

        view.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), icon), &cells);
        let action = view.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), icon), &cells);
        assert!(matches!(action, Some(ViewAction::CopyCode(text)) if text.as_ref() == source));
    }
}

#[test]
fn pending_copy_confirmation_survives_reflow_for_its_own_code_block() {
    let markdown = "- nested\n  ```text\nsame\n  ```\n\n```text\nsame\n```\n\n```text\nsame\n```";
    let (mut view, cells, buffer) = transcript(markdown, /*width*/ 40);
    let clicked = icons(&buffer)[2];
    view.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), clicked),
        &cells,
    );
    let action = view.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), clicked),
        &cells,
    );
    assert!(matches!(action, Some(ViewAction::CopyCode(text)) if text.as_ref() == "same\n"));
    assert_eq!(
        view.show_code_copy_result(&Ok(crate::clipboard_copy::CopyStatus::Pending(7))),
        None
    );

    let narrow_area = Rect::new(
        /*x*/ 2, /*y*/ 1, /*width*/ 9, /*height*/ 14,
    );
    let mut narrow = Buffer::empty(narrow_area);
    view.render(narrow_area, &mut narrow, &cells);
    assert_eq!(
        icons(&narrow).len(),
        2,
        "nested control is too narrow to render"
    );

    assert_eq!(
        view.finish_code_copy(
            &(7, Ok(crate::clipboard_copy::CopyStatus::Confirmed)),
            /*current*/ true,
        ),
        Some(std::time::Duration::from_secs(5))
    );
    let mut confirmed = Buffer::empty(narrow_area);
    view.render(narrow_area, &mut confirmed, &cells);
    let marks = (confirmed.area.top()..confirmed.area.bottom())
        .flat_map(|y| {
            (confirmed.area.left()..confirmed.area.right()).map(move |x| ScreenPosition::new(x, y))
        })
        .filter(|point| matches!(confirmed[*point].symbol(), "⎘" | "✓"))
        .collect::<Vec<_>>();
    assert_eq!(marks.len(), 2);
    assert_eq!(confirmed[marks[0]].symbol(), "⎘");
    assert_eq!(confirmed[marks[1]].symbol(), "✓");
}

#[test]
fn code_control_hover_press_and_release_outside_follow_button_semantics() {
    let (mut view, cells, mut buffer) = transcript("```text\ncontent\n```", /*width*/ 32);
    let icons = icons(&buffer);
    assert_eq!(
        icons.len(),
        1,
        "{}",
        crate::transcript_view::tests::text(&buffer)
    );
    let icon = icons[0];
    let area = view.area;

    assert!(matches!(
        view.handle_mouse(mouse(MouseEventKind::Moved, icon), &cells),
        Some(ViewAction::Changed)
    ));
    view.render(area, &mut buffer, &cells);
    assert!(buffer[icon].modifier.contains(Modifier::REVERSED));

    view.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), icon), &cells);
    view.render(area, &mut buffer, &cells);
    assert!(buffer[icon].modifier.contains(Modifier::UNDERLINED));

    let outside = ScreenPosition::new(icon.x.saturating_sub(2), icon.y);
    assert!(matches!(
        view.handle_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), outside),
            &cells
        ),
        Some(ViewAction::Changed)
    ));
    assert!(matches!(
        view.handle_mouse(
            mouse(MouseEventKind::Up(MouseButton::Left), outside),
            &cells
        ),
        Some(ViewAction::Changed)
    ));
}
