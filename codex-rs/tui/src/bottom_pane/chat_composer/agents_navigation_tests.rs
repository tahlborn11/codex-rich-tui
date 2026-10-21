use super::tests::new_test_composer;
use super::tests::snapshot_composer_state_with_width;
use super::tests::type_chars_humanlike;
use super::*;
use assert_matches::assert_matches;
use pretty_assertions::assert_eq;

#[test]
fn parent_owned_thread_allows_local_commands() {
    for (command, expected) in [
        ("/agents", SlashCommand::Agents),
        ("/subagents", SlashCommand::MultiAgents),
        ("/side", SlashCommand::Side),
        ("/btw", SlashCommand::Btw),
        ("/diff ", SlashCommand::Diff),
        ("/daemon", SlashCommand::Daemon),
        ("/warnings", SlashCommand::Warnings),
    ] {
        let (mut composer, _rx) = new_test_composer();
        composer.set_parent_owned_thread();
        composer.set_text_content(command.to_string(), Vec::new(), Vec::new());

        assert_eq!(
            composer.handle_submission(/*should_queue*/ false).0,
            InputResult::Command(expected)
        );
    }

    for (command, args) in [
        (SlashCommand::Title, "model"),
        (SlashCommand::Statusline, "model --no-colors"),
    ] {
        let (mut composer, _rx) = new_test_composer();
        composer.set_parent_owned_thread();
        let name = command.command();
        composer.set_text_content(format!("/{name} {args}"), Vec::new(), Vec::new());

        assert_eq!(
            composer.handle_key_event(KeyCode::Enter.into()).0,
            InputResult::CommandWithArgs(command, args.into(), Vec::new())
        );
    }
}

#[test]
fn parent_owned_thread_allows_safe_command_selected_from_prefix() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_parent_owned_thread();
    type_chars_humanlike(&mut composer, &['/', 'a', 'g']);

    let result = composer
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .0;

    assert_eq!(result, InputResult::Command(SlashCommand::Agents));
}

#[test]
fn left_navigates_only_from_an_empty_prompt() {
    let (mut composer, mut events) = new_test_composer();
    composer.set_agents_navigation_enabled(/*enabled*/ true);
    composer.insert_str("ab");
    composer.handle_key_event(KeyCode::Left.into());
    composer.insert_str("!");
    assert_eq!(composer.current_text(), "a!b");
    assert!(events.try_recv().is_err());

    composer.clear_for_ctrl_c();
    for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
        composer.handle_key_event(KeyEvent::new_with_kind(
            KeyCode::Left,
            KeyModifiers::NONE,
            kind,
        ));
    }
    assert!(events.try_recv().is_err());
    composer.handle_key_event(KeyCode::Left.into());
    assert_matches!(events.try_recv(), Ok(AppEvent::OpenAgentsOverview));
}

#[test]
fn left_respects_attachments_pastes_and_other_input_surfaces() {
    let setups: [fn(&mut ChatComposer); 4] = [
        |composer| composer.config = ChatComposerConfig::plain_text(),
        |composer| composer.set_remote_image_urls(vec!["https://example.com/image.png".into()]),
        |composer| {
            composer.handle_key_event(KeyCode::Char('x').into());
        },
        |composer| {
            composer.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        },
    ];
    for setup in setups {
        let (mut composer, mut events) = new_test_composer();
        composer.set_agents_navigation_enabled(/*enabled*/ true);
        setup(&mut composer);
        composer.handle_key_event(KeyCode::Left.into());
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, AppEvent::OpenAgentsOverview))
        );
    }
}

#[test]
fn agents_navigation_hint_snapshots() {
    for name in [
        "agents_navigation_enabled",
        "agents_navigation_help",
        "agents_navigation_status",
    ] {
        snapshot_composer_state_with_width(
            name,
            /*width*/ 80,
            /*enhanced_keys_supported*/ false,
            |composer| {
                composer.set_agents_navigation_enabled(/*enabled*/ true);
                match name {
                    "agents_navigation_help" => {
                        composer.handle_key_event(KeyCode::Char('?').into());
                    }
                    "agents_navigation_status" => {
                        composer.set_status_line_enabled(/*enabled*/ true);
                        composer.set_status_line(Some(Line::from("model · project")));
                    }
                    _ => {}
                }
            },
        );
    }
}

#[test]
fn left_respects_editor_and_vim_history_remaps() {
    use codex_config::types::KeybindingSpec;
    use codex_config::types::KeybindingsSpec;
    use codex_config::types::TuiKeymap;

    for vim in [false, true] {
        let (mut composer, mut events) = new_test_composer();
        composer.set_agents_navigation_enabled(/*enabled*/ true);
        composer.insert_str("previous");
        composer.handle_key_event(KeyCode::Enter.into());
        let mut config = TuiKeymap::default();
        if vim {
            config.vim_normal.move_left = Some(KeybindingsSpec::One(KeybindingSpec("h".into())));
            config.vim_normal.move_up = Some(KeybindingsSpec::One(KeybindingSpec("left".into())));
        } else {
            config.editor.move_left = Some(KeybindingsSpec::One(KeybindingSpec("ctrl-b".into())));
            config.editor.move_up = Some(KeybindingsSpec::One(KeybindingSpec("left".into())));
        }
        composer.set_keymap_bindings(&RuntimeKeymap::from_config(&config).unwrap());
        composer.set_vim_enabled(vim);
        assert!(composer.footer_props().key_hints.agents.is_none());
        composer.handle_key_event(KeyCode::Left.into());
        assert_eq!(composer.current_text(), "previous");
        assert!(events.try_recv().is_err());
    }
}

#[test]
fn right_opens_outputs_only_from_an_empty_prompt() {
    let (mut composer, mut events) = new_test_composer();
    composer.insert_str("ab");
    composer.handle_key_event(KeyCode::Left.into());
    composer.handle_key_event(KeyCode::Right.into());
    composer.insert_str("!");
    assert_eq!(composer.current_text(), "ab!");
    assert!(events.try_recv().is_err());
    composer.clear_for_ctrl_c();
    for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
        composer.handle_key_event(KeyEvent::new_with_kind(
            KeyCode::Right,
            KeyModifiers::NONE,
            kind,
        ));
    }
    assert!(events.try_recv().is_err());
    composer.handle_key_event(KeyCode::Right.into());
    assert_matches!(events.try_recv(), Ok(AppEvent::OpenOutputs));
}

#[test]
fn right_outputs_respects_attachments_and_editor_and_vim_remaps() {
    let (mut composer, mut events) = new_test_composer();
    composer.set_remote_image_urls(vec!["https://example.com/image.png".into()]);
    composer.handle_key_event(KeyCode::Right.into());
    assert!(events.try_recv().is_err());

    use codex_config::types::KeybindingSpec;
    use codex_config::types::KeybindingsSpec;
    use codex_config::types::TuiKeymap;

    for vim in [false, true] {
        let (mut composer, mut events) = new_test_composer();
        let mut config = TuiKeymap::default();
        if vim {
            config.vim_normal.move_right =
                Some(KeybindingsSpec::One(KeybindingSpec("ctrl-f".into())));
        } else {
            config.editor.move_right = Some(KeybindingsSpec::One(KeybindingSpec("ctrl-f".into())));
        }
        composer.set_keymap_bindings(&RuntimeKeymap::from_config(&config).unwrap());
        composer.set_vim_enabled(vim);
        assert!(composer.footer_props().key_hints.outputs.is_none());
        composer.handle_key_event(KeyCode::Right.into());
        assert!(events.try_recv().is_err());
    }
}

#[test]
fn right_dismisses_shortcut_help_before_opening_outputs() {
    let (mut composer, mut events) = new_test_composer();
    composer.handle_key_event(KeyCode::Char('?').into());
    assert!(composer.shortcut_overlay_visible());
    composer.handle_key_event(KeyCode::Right.into());
    assert!(!composer.shortcut_overlay_visible());
    assert!(events.try_recv().is_err());
    composer.handle_key_event(KeyCode::Right.into());
    assert_matches!(events.try_recv(), Ok(AppEvent::OpenOutputs));
}

#[test]
fn outputs_hint_matches_navigation_availability() {
    let (mut composer, _events) = new_test_composer();
    assert!(composer.footer_props().key_hints.outputs.is_some());

    composer.insert_str("draft");
    assert!(composer.footer_props().key_hints.outputs.is_none());
    composer.clear_for_ctrl_c();

    composer.set_remote_image_urls(vec!["https://example.com/image.png".into()]);
    assert!(composer.footer_props().key_hints.outputs.is_none());
    composer.set_remote_image_urls(Vec::new());

    composer.handle_key_event(KeyCode::Char('?').into());
    assert!(composer.footer_props().key_hints.outputs.is_none());
    composer.handle_key_event(KeyCode::Right.into());
    assert!(!composer.shortcut_overlay_visible());

    let config: codex_config::types::TuiKeymap =
        toml::from_str("[editor]\nmove_right = 'ctrl-f'").unwrap();
    let runtime = crate::keymap::RuntimeKeymap::from_config(&config).unwrap();
    composer.set_keymap_bindings(&runtime);
    assert!(composer.footer_props().key_hints.outputs.is_none());
}
