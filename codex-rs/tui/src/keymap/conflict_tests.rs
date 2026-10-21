//! Regression coverage for conflict diagnostics and validation pass ordering.

use super::RuntimeKeymap;
use codex_config::types::TuiKeymap;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

#[test]
fn conflicting_contexts_report_the_first_conflict_in_validation_order() {
    let contexts = [
        ("editor", "insert_newline", "yank"),
        ("vim_normal", "enter_insert", "cancel_operator"),
        ("vim_operator", "delete_line", "cancel"),
        ("vim_text_object", "word", "cancel"),
        ("pager", "scroll_up", "close_transcript"),
        ("list", "move_up", "cancel"),
        ("agents", "search", "toggle_grouping"),
        ("approval", "open_fullscreen", "cancel"),
    ];
    let mut config = serde_json::Map::new();
    for (context, first, second) in contexts {
        config.insert(context.to_string(), json!({first: "f12", second: "f12"}));
    }

    for (context, first, second) in contexts {
        let keymap: TuiKeymap = serde_json::from_value(Value::Object(config.clone()))
            .expect("valid keymap configuration");
        assert_eq!(
            RuntimeKeymap::from_config(&keymap).expect_err("expected binding conflict"),
            format!(
                "Ambiguous `tui.keymap.{context}` bindings: `{first}` and `{second}` use the same key. \
Set unique keys in `~/.codex/config.toml` and retry. \
See the Codex keymap documentation for supported actions and examples."
            )
        );
        config.remove(context);
    }
}

#[test]
fn new_agents_defaults_preserve_existing_custom_bindings() {
    for (action, alias) in [
        ("archive", "a"),
        ("delete", "backspace"),
        ("hide", "h"),
        ("new_worktree", "w"),
        ("fork", "f"),
    ] {
        for (context, existing, suffix) in [
            ("agents", "stop", ""),
            ("agents", "search", ""),
            ("list", "move_down", ""),
            ("global", "copy", ""),
            ("agents", "stop", " f12"),
            ("list", "move_down", " f12"),
        ] {
            // Plain keys are allowed in the dashboard, but global actions and list
            // chord prefixes still share text-entry surfaces. Backspace is reserved
            // for editing task input except when bound to task deletion.
            if (alias == "backspace" && context == "agents")
                || (alias != "backspace"
                    && (context == "global" || context == "list" && !suffix.is_empty()))
            {
                continue;
            }
            let keymap: TuiKeymap = serde_json::from_value(
                json!({context: {existing: format!("{alias}{suffix}")}, "approval": {"open_fullscreen": "f12", "approve_for_session": []},
                    "editor": {"move_line_start": [], "move_line_end": [], "delete_backward": [], "delete_backward_word": [], "delete_forward": []}}),
            )
            .unwrap();
            let runtime =
                RuntimeKeymap::from_config(&keymap).expect("existing configuration remains valid");
            let bindings = match action {
                "archive" => &runtime.agents.archive,
                "delete" => &runtime.agents.delete,
                "hide" => &runtime.agents.hide,
                "new_worktree" => &runtime.agents.new_worktree,
                "fork" => &runtime.agents.fork,
                _ => unreachable!(),
            };
            assert!(
                bindings.is_empty(),
                "{action} shadows {context}.{existing} = {alias}{suffix}"
            );
        }
    }
}

#[test]
fn activity_default_preserves_configured_keys_and_prefixes() {
    use crate::key_hint;
    use crossterm::event::KeyCode;

    for (context, action) in [
        ("global", "copy"),
        ("editor", "move_left"),
        ("list", "move_up"),
    ] {
        for configured in ["f4", "f4 f12"] {
            let keymap: TuiKeymap =
                serde_json::from_value(json!({context: {action: configured}})).unwrap();
            let runtime = RuntimeKeymap::from_config(&keymap)
                .expect("a new activity default must preserve existing configuration");
            assert!(
                !super::user_bindings(&runtime.app.focus_activity)
                    .contains(&key_hint::plain(KeyCode::F(4)))
            );
        }
    }
}

#[test]
fn explicit_activity_remapping_and_unbinding_replace_all_defaults() {
    use crate::key_hint;
    use crossterm::event::KeyCode;

    for (configured, expected, hint) in [
        (
            json!("f12"),
            vec![key_hint::plain(KeyCode::F(12))],
            Some("f12"),
        ),
        (
            json!("page-up"),
            vec![key_hint::plain(KeyCode::PageUp)],
            Some("pgup"),
        ),
        (json!([]), Vec::new(), None),
        (json!("ctrl-x t"), Vec::new(), Some("⌃x t")),
    ] {
        let keymap: TuiKeymap =
            serde_json::from_value(json!({"global": {"focus_activity": configured}})).unwrap();
        let runtime = RuntimeKeymap::from_config(&keymap).expect("valid explicit activity binding");
        assert_eq!(super::user_bindings(&runtime.app.focus_activity), expected);
        assert_eq!(
            runtime
                .primary_hint(super::KeymapContext::Global, "focus_activity")
                .map(crate::key_hint::ShortcutHint::display_label),
            hint.map(str::to_string),
        );
    }
}

#[test]
fn warnings_defaults_preserve_custom_keys_and_chord_prefixes() {
    for binding in ["f2", "f2 f12"] {
        let keymap: TuiKeymap = serde_json::from_value(json!({
            "editor": {"move_left": binding}
        }))
        .unwrap();
        let runtime = RuntimeKeymap::from_config(&keymap).expect("existing binding remains valid");
        assert!(runtime.app.open_warnings.is_empty());
    }
}

#[test]
fn agents_search_keeps_unshadowed_fallbacks() {
    use crate::key_hint;
    use crossterm::event::KeyCode;

    for (configured, remaining) in [("/", KeyCode::F(3)), ("f3", KeyCode::Char('/'))] {
        let keymap: TuiKeymap =
            serde_json::from_value(json!({"list": {"move_up": configured}})).unwrap();
        let runtime = RuntimeKeymap::from_config(&keymap).unwrap();
        assert_eq!(runtime.agents.search, vec![key_hint::plain(remaining)]);
    }
}

#[test]
fn outputs_actions_have_an_independent_remappable_context() {
    let config: codex_config::types::TuiKeymap =
        toml::from_str("[global]\nopen_outputs = 'f12'\n[outputs]\nmove_down = 'ctrl-j'\n")
            .unwrap();
    let runtime = super::RuntimeKeymap::from_config(&config).unwrap();
    assert_eq!(
        runtime.app.open_outputs,
        vec![crate::key_hint::plain(crossterm::event::KeyCode::F(12))]
    );
    assert_eq!(
        runtime.outputs.move_down,
        vec![crate::key_hint::ctrl(crossterm::event::KeyCode::Char('j'))]
    );
    assert_ne!(runtime.outputs.move_down, runtime.list.move_down);
}

#[test]
fn outputs_global_binding_cannot_steal_editor_input() {
    for binding in ["right", "ctrl-t", "a"] {
        let config: codex_config::types::TuiKeymap =
            toml::from_str(&format!("[global]\nopen_outputs = '{binding}'\n",)).unwrap();
        assert!(
            super::RuntimeKeymap::from_config(&config).is_err(),
            "{binding}"
        );
    }
}

#[test]
fn outputs_navigation_cannot_shadow_document_controls() {
    let config: codex_config::types::TuiKeymap =
        toml::from_str("[outputs]\nmove_down = 'r'").unwrap();
    let error = super::RuntimeKeymap::from_config(&config).unwrap_err();
    assert!(error.contains("outputs.refresh"));
}

#[test]
fn outputs_fixed_controls_reject_navigation_bindings_and_chords() {
    for key in ["alt-left", "alt-right", "a", "1", "2", "3", "4", "5", "esc"] {
        for binding in [
            key.to_owned(),
            format!("ctrl-x {key}"),
            format!("{key} f12"),
        ] {
            let config: TuiKeymap = serde_json::from_value(json!({
                "outputs": {"move_down": binding, "cancel": []}
            }))
            .unwrap();
            assert!(RuntimeKeymap::from_config(&config).is_err(), "{binding}");
        }
    }
}

#[test]
fn outputs_cancel_can_use_fixed_close_keys() {
    let config: TuiKeymap = serde_json::from_value(json!({
        "outputs": {"cancel": ["esc", "ctrl-c"]}
    }))
    .unwrap();
    assert!(RuntimeKeymap::from_config(&config).is_ok());
}
