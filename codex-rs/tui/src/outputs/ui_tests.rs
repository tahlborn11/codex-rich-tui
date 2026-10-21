use super::*;

#[test]
fn agent_preview_keeps_status_and_summary_as_literal_text() {
    let agent = Agent {
        id: "child".into(),
        title: "Investigate parser".into(),
        status: "running".into(),
        summary: "Still reading `#` and +diff text.".into(),
    };
    let preview = agent_preview(&agent);
    assert!(preview.contains("Investigate parser"));
    assert!(preview.contains("Status: running"));
    assert!(preview.contains("`#` and +diff text"));
}

#[test]
fn category_order_matches_the_visible_navigation_order() {
    assert_eq!(
        Category::ALL,
        [
            Category::Plans,
            Category::Files,
            Category::Changes,
            Category::Agents
        ]
    );
}

struct Fixture {
    view: OutputsView,
    _client: codex_app_server_client::RemoteAppServerClient,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture() -> Fixture {
    fixture_with_history(Vec::new(), "latest").await
}
async fn fixture_with_history(items: Vec<serde_json::Value>, latest: &str) -> Fixture {
    let latest = latest.to_owned();
    use codex_app_server_client::RemoteAppServerClient;
    use codex_app_server_client::RemoteAppServerConnectArgs;
    use codex_app_server_client::RemoteAppServerEndpoint;
    use futures::SinkExt;
    use futures::StreamExt;
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let request: serde_json::Value = serde_json::from_str(&text).unwrap();
            if request.get("id").is_none() {
                continue;
            }
            let result = if request["method"] == "initialize" {
                json!({"userAgent":"outputs-test"})
            } else if request["method"] == "thread/items/list" {
                json!({"data":items,"nextCursor":null})
            } else if request["method"] == "thread/turns/list" {
                json!({"data":[{"id":latest,"status":"completed","items":[]}],"nextCursor":null})
            } else {
                json!({"data":[],"nextCursor":null})
            };
            if socket
                .send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let client = RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
        endpoint: RemoteAppServerEndpoint::WebSocket {
            websocket_url: url,
            auth_token: None,
        },
        client_name: "outputs-test".into(),
        client_version: "0.0.0".into(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: 16,
    })
    .await
    .unwrap();
    let keymap = crate::keymap::RuntimeKeymap::defaults();
    let mut view = OutputsView::new(
        "chat-a".into(),
        PathBuf::from("/project"),
        None,
        AppServerRequestHandle::Remote(client.request_handle()),
        keymap.outputs,
        keymap.pager,
        FrameRequester::test_dummy(),
        None,
    );
    view.cancel_loads();
    Fixture {
        view,
        _client: client,
        server,
    }
}
fn repo(branch: &str) -> Repository {
    Repository {
        root: PathBuf::from("/project/api"),
        identity: "/project/api".into(),
        branch: Some(branch.into()),
        head: Some("a".repeat(40)),
        branches: vec!["main".into(), branch.into()],
        default_base: Some("main".into()),
    }
}
fn doc(id: &str, title: &str, markdown: &str) -> Document {
    Document {
        id: id.into(),
        title: title.into(),
        markdown: markdown.into(),
    }
}
fn buffer_text(buffer: &Buffer) -> String {
    buffer
        .content()
        .chunks(usize::from(buffer.area.width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn filtered_plan_previews_follow_stable_identity() {
    let mut f = fixture().await;
    f.view.set_conversation(ConversationData {
        plans: vec![
            doc("a", "First", "WRONG"),
            doc("b", "Design", "# Correct document"),
        ],
        ..Default::default()
    });
    f.view.query = "design".into();
    assert_eq!(f.view.items().len(), 1);
    assert_eq!(f.view.preview(), "# Correct document");
    let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 16));
    f.view.render(buffer.area, &mut buffer);
    let text = buffer_text(&buffer);
    assert!(text.contains("Correct document"));
    assert!(!text.contains("WRONG"));
    for (width, height) in [(45, 12), (8, 2), (1, 1)] {
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        f.view.focus = Focus::Preview;
        f.view.render(buffer.area, &mut buffer);
    }
}

#[tokio::test]
async fn initial_catalog_survives_category_navigation_and_remembers_refs() {
    let mut f = fixture().await;
    let generation = f.view.generation();
    f.view.cycle_category(1);
    f.view
        .load_tx
        .send(LoadResult::Repositories(
            generation,
            Ok(vec![repo("feature/auth")]),
        ))
        .unwrap();
    f.view.drain_loads();
    assert_eq!(f.view.repositories.len(), 1);
    assert_eq!(f.view.target_ref.as_deref(), Some("feature/auth"));
    f.view.set_repositories(vec![repo("feature/billing")]);
    assert_eq!(f.view.target_ref.as_deref(), Some("feature/auth"));
    assert_eq!(
        f.view.selected_repository().unwrap().branch.as_deref(),
        Some("feature/billing")
    );
    let mut buffer = Buffer::empty(Rect::new(0, 0, 180, 3));
    f.view.category = Category::Changes;
    f.view.render_context(buffer.area, &mut buffer);
    let text = buffer_text(&buffer);
    assert!(text.contains("feature/auth"));
    assert!(text.contains("feature/billing"));
    assert!(text.contains("differs from checkout"));
}

#[tokio::test]
async fn late_agent_result_cannot_replace_new_selection() {
    let mut f = fixture().await;
    f.view.category = Category::Agents;
    f.view.conversation.agents = vec![
        Agent {
            id: "a".into(),
            title: "First".into(),
            status: "working".into(),
            summary: String::new(),
        },
        Agent {
            id: "b".into(),
            title: "Second".into(),
            status: "working".into(),
            summary: String::new(),
        },
    ];
    f.view.load_selected_preview();
    let stale = f.view.detail_generation;
    f.view.selected = 1;
    f.view.load_selected_preview();
    let current = f.view.detail_generation;
    assert_ne!(stale, current);
    f.view
        .load_tx
        .send(LoadResult::Agent(current, Ok("SECOND".into())))
        .unwrap();
    f.view
        .load_tx
        .send(LoadResult::Agent(stale, Ok("WRONG".into())))
        .unwrap();
    f.view.drain_loads();
    assert_eq!(f.view.preview(), "SECOND");
}

#[tokio::test]
async fn change_files_are_selected_without_reloading_or_changing_modes() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    let patch = "diff --git a/first b/first\n--- a/first\n+++ b/first\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/second b/second\n--- a/second\n+++ b/second\n@@ -1 +1 @@\n-before\n+after\n";
    f.view.set_change_text(Ok(patch.into()));
    assert_eq!(f.view.items().len(), 2);
    assert!(f.view.preview().contains("+new"));
    let generation = f.view.detail_generation;
    f.view.selected = 1;
    f.view.load_selected_preview();
    assert_eq!(f.view.detail_generation, generation);
    assert_eq!(f.view.change_kind, ChangeKind::Branch);
    assert!(f.view.preview().contains("+after"));
    assert!(!f.view.preview().contains("+new"));
    f.view.query = "first".into();
    f.view.clamp_selection();
    assert!(f.view.preview().contains("+new"));
    f.view.cycle_change_kind(1);
    assert_eq!(f.view.change_kind, ChangeKind::Unstaged);
    assert!(f.view.change_files.is_empty());
    assert!(!f.view.preview().contains("+new"));
}

#[tokio::test]
async fn changed_file_results_reject_stale_modes_and_report_empty_changes() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    let stale = f.view.detail_generation;
    f.view.cycle_change_kind(1);
    f.view
        .load_tx
        .send(LoadResult::Change(
            stale,
            Ok("diff --git a/old b/old\n-old".into()),
        ))
        .unwrap();
    f.view.drain_loads();
    assert!(f.view.change_files.is_empty());
    f.view.set_change_text(Ok(String::new()));
    assert_eq!(f.view.preview(), "No changes in this view.");
}

#[tokio::test]
async fn previews_render_markdown_files_and_keep_source_exact() {
    let mut f = fixture().await;
    f.view.category = Category::Files;
    f.view.file_text = Some("# Heading\n\nBody".into());
    f.view.preview_path = Some(PathBuf::from("/project/design.md"));
    let text = f.view.preview_text(&f.view.preview(), 60).to_string();
    assert!(text.contains("Heading"));
    assert!(!text.contains("# Heading"));
    f.view.source_mode = true;
    assert!(
        f.view
            .preview_text(&f.view.preview(), 60)
            .to_string()
            .contains("# Heading")
    );
    assert_eq!(f.view.preview(), "# Heading\n\nBody");
    f.view.category = Category::Changes;
    f.view.source_mode = false;
    f.view.change_text = Some("--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new".into());
    let text = f.view.preview_text(&f.view.preview(), 60).to_string();
    assert!(text.contains("+new"));
    assert!(text.contains("@@ -1 +1 @@"));
}

#[tokio::test]
async fn list_and_preview_scroll_remain_bounded_and_find_content() {
    let mut f = fixture().await;
    f.view.conversation.plans = (0..50)
        .map(|i| {
            doc(
                &i.to_string(),
                &format!("Plan {i}"),
                "first\nsecond\nneedle\nlast",
            )
        })
        .collect();
    f.view.selected = 49;
    let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 6));
    f.view.render_list(buffer.area, &mut buffer);
    assert!(buffer_text(&buffer).contains("Plan 49"));
    f.view.preview_scroll = usize::MAX;
    f.view.render_preview(buffer.area, &mut buffer);
    assert!(f.view.preview_scroll < 10);
    f.view.find_query = "needle".into();
    f.view.find_in_preview();
    assert!(f.view.notice.as_deref().unwrap().contains("Found"));
}

#[tokio::test]
async fn list_and_preview_can_scroll_beyond_u16_offsets() {
    let mut f = fixture().await;
    f.view.conversation.plans = (0..70_002)
        .map(|i| doc(&i.to_string(), &format!("Plan {i}"), "body"))
        .collect();
    f.view.selected = 70_001;
    let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 6));
    f.view.render_list(buffer.area, &mut buffer);
    assert!(buffer_text(&buffer).contains("Plan 70001"));

    f.view.conversation.plans = vec![doc(
        "tail",
        "Large",
        &("x\n".repeat(70_000) + "tail needle"),
    )];
    f.view.selected = 0;
    f.view.source_mode = true;
    f.view.find_query = "tail needle".into();
    f.view.preview_width = 40;
    f.view.find_in_preview();
    assert_eq!(f.view.preview_scroll, 70_000);
    buffer.reset();
    f.view.render_preview(buffer.area, &mut buffer);
    assert!(buffer_text(&buffer).contains("tail needle"));

    f.view.conversation.plans[0].markdown = "x".repeat(100_000) + " tail needle";
    f.view.preview_width = 10;
    f.view.find_in_preview();
    assert!(f.view.preview_scroll > 9_000);
    buffer = Buffer::empty(Rect::new(0, 0, 10, 6));
    f.view.render_preview(buffer.area, &mut buffer);
    assert!(buffer_text(&buffer).contains("needle"));
    f.view.preview_scroll = usize::MAX;
    buffer = Buffer::empty(Rect::new(0, 0, 1, 10));
    f.view.render_preview(buffer.area, &mut buffer);
    assert!(f.view.preview_scroll > usize::from(u16::MAX));
    assert!(buffer_text(&buffer).contains('e'));
}

#[tokio::test]
async fn directory_results_share_a_total_entry_budget() {
    let mut f = fixture().await;
    f.view.directories.insert(
        PathBuf::from("/project/full"),
        (0..MAX_DIRECTORY_CACHE_ENTRIES - 1)
            .map(|i| directory_entry(&format!("/project/full/{i}"), false))
            .collect(),
    );
    f.view
        .load_tx
        .send(LoadResult::Directory(
            f.view.generation,
            PathBuf::from("/project/next"),
            Ok(data::DirectoryListing {
                entries: vec![
                    directory_entry("/project/next/a", false),
                    directory_entry("/project/next/b", false),
                ],
                truncated: false,
            }),
        ))
        .unwrap();
    f.view.drain_loads();
    assert_eq!(
        f.view.directories.values().map(Vec::len).sum::<usize>(),
        MAX_DIRECTORY_CACHE_ENTRIES
    );
    assert!(
        f.view
            .notice
            .as_deref()
            .unwrap()
            .contains("File entry limit")
    );
}

#[tokio::test]
async fn ctrl_c_closes_and_copy_completions_match_pending_request() {
    let mut f = fixture().await;
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    f.view.handle_key(
        &mut tui,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    );
    assert!(f.view.is_done);
    assert!(f.view.pending_copy.is_none());
    f.view.pending_copy = Some(42);
    assert!(
        f.view
            .finish_clipboard(&(41, Ok(CopyStatus::Confirmed)))
            .is_none()
    );
    assert!(
        f.view
            .finish_clipboard(&(42, Err("clipboard unavailable".into())))
            .is_some()
    );
    assert!(f.view.pending_copy.is_none());
    assert!(
        f.view
            .notice
            .as_deref()
            .unwrap()
            .contains("clipboard unavailable")
    );
}

#[tokio::test]
async fn unavailable_remembered_checkout_requires_explicit_selection() {
    let mut f = fixture().await;
    f.view
        .preferences
        .set_selected_repo(Some("/missing".into()));
    f.view.set_repositories(vec![repo("main")]);
    assert!(f.view.selected_repository().is_none());
    assert_eq!(f.view.preferences.selected_repo(), Some("/missing"));
    f.view.cycle_repository(1);
    assert_eq!(
        f.view.selected_repository().unwrap().identity,
        "/project/api"
    );
}

#[tokio::test]
async fn new_turn_without_patch_suppresses_previous_persisted_patch() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    f.view.change_kind = ChangeKind::TurnPatch;
    f.view.conversation.turn_patch = Some("old persisted patch".into());
    f.view.set_turn_patch(Some("new patch"));
    assert_eq!(f.view.preview(), "new patch");
    f.view.set_turn_patch(Some(""));
    assert!(f.view.preview().contains("No patch recorded"));
    assert!(!f.view.preview().contains("old persisted patch"));
}

#[tokio::test]
async fn labels_and_notices_do_not_emit_terminal_escape_sequences() {
    let mut f = fixture().await;
    f.view.set_conversation(ConversationData {
        plans: vec![doc("a", "\x1b[31munsafe", "safe")],
        ..Default::default()
    });
    f.view.notice = Some("\x1b]52;c;secret\x07".into());
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 20));
    f.view.render(buffer.area, &mut buffer);
    let text = buffer_text(&buffer);
    assert!(!text.contains('\x1b'));
    assert!(!text.contains('\x07'));
}

fn thread_metadata(environment: &str) -> codex_app_server_protocol::Thread {
    serde_json::from_value(serde_json::json!({
        "id":"chat-a", "sessionId":"s", "preview":"", "ephemeral":false,
        "modelProvider":"openai", "createdAt":0, "updatedAt":0,
        "status":{"type":"idle"}, "cwd":"/authoritative-project", "cliVersion":"test",
        "source":"cli", "turns":[],
        "environments":[{"environmentId":environment,"cwd":"/authoritative-project","runtimeWorkspaceRoots":[]}]
    })).unwrap()
}

#[tokio::test]
async fn workspace_verification_uses_chat_environment_and_authoritative_cwd() {
    let mut f = fixture().await;
    f.view.set_supported_environments(vec!["local".into()]);
    f.view
        .load_tx
        .send(LoadResult::Workspace(
            f.view.generation,
            Ok(Box::new(thread_metadata("local"))),
        ))
        .unwrap();
    f.view.drain_loads();
    assert!(f.view.workspace_verified);
    assert_eq!(f.view.cwd, PathBuf::from("/authoritative-project"));
    f.view.clear_workspace();
    f.view
        .load_tx
        .send(LoadResult::Workspace(
            f.view.generation,
            Ok(Box::new(thread_metadata("remote"))),
        ))
        .unwrap();
    f.view.drain_loads();
    assert!(!f.view.workspace_verified);
    assert!(f.view.repositories.is_empty());
    assert!(f.view.conversation.files.is_empty());
}

#[tokio::test]
async fn outputs_wide_and_narrow_layouts() {
    let mut f = fixture().await;
    f.view.set_conversation(ConversationData { plans: vec![doc("design", "Viewer plan", "# Outputs\n\n- [x] Browse plans\n- [ ] Compare branches\n\n```rust\nlet answer = 42;\n```")], ..Default::default() });
    let mut wide = Buffer::empty(Rect::new(0, 0, 100, 18));
    f.view.render(wide.area, &mut wide);
    insta::assert_snapshot!("outputs_wide", buffer_text(&wide));
    f.view.focus = Focus::Preview;
    let mut narrow = Buffer::empty(Rect::new(0, 0, 60, 18));
    f.view.render(narrow.area, &mut narrow);
    insta::assert_snapshot!("outputs_narrow", buffer_text(&narrow));
}

#[tokio::test]
async fn relative_history_paths_use_verified_cwd_in_both_response_orders() {
    for history_first in [true, false] {
        let mut f = fixture().await;
        f.view.set_supported_environments(vec!["local".into()]);
        let history = ConversationData {
            files: vec![PathBuf::from("plan.md")],
            ..Default::default()
        };
        if history_first {
            f.view.set_conversation(history.clone());
        }
        f.view
            .load_tx
            .send(LoadResult::Workspace(
                f.view.generation,
                Ok(Box::new(thread_metadata("local"))),
            ))
            .unwrap();
        f.view.drain_loads();
        if !history_first {
            f.view.set_conversation(history);
        }
        assert_eq!(
            f.view.conversation.files,
            vec![PathBuf::from("/authoritative-project/plan.md")]
        );
    }
}

#[tokio::test]
async fn recorded_outputs_exclude_failed_edits_and_previous_turn_patches() {
    let items = ["completed", "failed", "declined"].into_iter().map(|status| serde_json::json!({
        "turnId":"old", "item":{"type":"fileChange", "id":status, "status":status,
        "changes":[{"path":format!("{status}.md"), "kind":{"type":"add"}, "diff":"+applied"}]}
    })).collect();
    let f = fixture_with_history(items, "latest").await;
    let outputs = data::load_conversation(&f.view.handle, "chat-a")
        .await
        .unwrap();
    assert_eq!(outputs.files, vec![PathBuf::from("completed.md")]);
    assert!(outputs.turn_patch.is_none());
}

#[tokio::test]
async fn changed_files_wide_and_narrow_layouts() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    f.view.workspace_verified = true;
    f.view.set_repositories(vec![repo("feature/viewer")]);
    f.view.set_change_text(Ok("diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"before\");\n+    println!(\"after\");\n }\ndiff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-Old heading\n+New heading\n".into()));
    // Expand the source folder, then select its file for the split preview.
    f.view.toggle_directory();
    f.view.move_right();
    let mut wide = Buffer::empty(Rect::new(0, 0, 180, 20));
    f.view.render(wide.area, &mut wide);
    insta::assert_snapshot!("changes_files_wide", buffer_text(&wide));
    f.view.focus = Focus::Preview;
    let mut narrow = Buffer::empty(Rect::new(0, 0, 60, 20));
    f.view.render(narrow.area, &mut narrow);
    insta::assert_snapshot!("changes_files_narrow", buffer_text(&narrow));
}

#[tokio::test]
async fn category_tabs_are_visible_highlighted_and_clickable() {
    let mut f = fixture().await;
    let mut buffer = Buffer::empty(Rect::new(0, 0, 60, 3));
    f.view.render_header(buffer.area, &mut buffer);
    let text = buffer_text(&buffer);
    for category in Category::ALL {
        assert!(text.contains(category.label()));
    }
    let (plans, _) = f.view.tab_hit_areas[0];
    assert!(
        buffer[(plans.x + 1, plans.y)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    let (changes, _) = *f
        .view
        .tab_hit_areas
        .iter()
        .find(|(_, category)| *category == Category::Changes)
        .unwrap();
    f.view.focus = Focus::Preview;
    assert!(f.view.activate_tab(changes.x, changes.y));
    assert_eq!(f.view.category, Category::Changes);
    assert_eq!(f.view.focus, Focus::List);
    assert!(!f.view.activate_tab(59, 2));
    f.view.render_header(buffer.area, &mut buffer);
    assert!(
        buffer[(changes.x + 1, changes.y)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    let mut tiny = Buffer::empty(Rect::new(0, 0, 10, 3));
    f.view.render_header(tiny.area, &mut tiny);
    assert!(buffer_text(&tiny).contains("Changes"));
    assert_eq!(f.view.tab_hit_areas.len(), 1);
}

fn directory_entry(path: &str, is_dir: bool) -> data::DirectoryEntry {
    data::DirectoryEntry {
        path: PathBuf::from(path),
        is_dir,
    }
}

#[tokio::test]
async fn file_tree_expands_collapses_and_preserves_relative_labels() {
    let mut f = fixture().await;
    f.view.category = Category::Files;
    f.view.directories.insert(
        PathBuf::from("/project"),
        vec![
            directory_entry("/project/src", true),
            directory_entry("/project/README.md", false),
        ],
    );
    f.view.directories.insert(
        PathBuf::from("/project/src"),
        vec![
            directory_entry("/project/src/nested", true),
            directory_entry("/project/src/main.rs", false),
        ],
    );
    f.view.directories.insert(
        PathBuf::from("/project/src/nested"),
        vec![directory_entry("/project/src/nested/lib.rs", false)],
    );
    assert_eq!(f.view.items().len(), 2);
    assert!(f.view.items()[0].1.contains("▸ src/"));
    assert!(
        !f.view
            .items()
            .iter()
            .any(|(_, label)| label.contains("/project"))
    );
    f.view.move_right();
    assert_eq!(f.view.category, Category::Files);
    assert_eq!(f.view.items().len(), 4);
    f.view.move_right();
    assert_eq!(
        f.view.selected_tree_row().unwrap().path,
        PathBuf::from("/project/src/nested")
    );
    f.view.toggle_directory();
    assert_eq!(f.view.items().len(), 5);
    f.view.move_right();
    assert_eq!(
        f.view.selected_tree_row().unwrap().path,
        PathBuf::from("/project/src/nested/lib.rs")
    );
    f.view.move_left();
    assert_eq!(
        f.view.selected_tree_row().unwrap().path,
        PathBuf::from("/project/src/nested")
    );
    f.view.move_left();
    assert_eq!(f.view.items().len(), 4);
    assert!(
        !f.view
            .expanded_files
            .contains(&PathBuf::from("/project/src/nested"))
    );
    f.view.query = "lib.rs".into();
    f.view.selected = 0;
    assert_eq!(f.view.items().len(), 3);
    assert!(f.view.items().last().unwrap().1.contains("lib.rs"));
    assert!(
        !f.view
            .expanded_files
            .contains(&PathBuf::from("/project/src/nested"))
    );
}

#[tokio::test]
async fn change_tree_selection_keeps_file_patch_identity() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    f.view.set_repositories(vec![repo("feature/tree")]);
    f.view.set_change_text(Ok("diff --git a/src/one.rs b/src/one.rs\n--- a/src/one.rs\n+++ b/src/one.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/src/nested/two.rs b/src/nested/two.rs\n--- a/src/nested/two.rs\n+++ b/src/nested/two.rs\n@@ -1 +1 @@\n-before\n+after\n".into()));
    let generation = f.view.detail_generation;
    assert_eq!(f.view.items().len(), 1);
    assert!(f.view.preview().contains("src/"));
    f.view.move_right();
    f.view.move_right(); // nested folder
    f.view.move_right(); // expand nested
    f.view.move_right(); // nested file
    assert!(f.view.preview().contains("+after"));
    assert!(!f.view.preview().contains("+new"));
    assert_eq!(f.view.detail_generation, generation);
    f.view.move_left();
    f.view.move_left();
    assert_eq!(f.view.items().len(), 3);
}

#[tokio::test]
async fn directory_results_preserve_selection_and_reject_previous_scope() {
    let mut f = fixture().await;
    f.view.category = Category::Files;
    let root = PathBuf::from("/project");
    f.view.directories.insert(
        root.clone(),
        vec![
            directory_entry("/project/a", true),
            directory_entry("/project/z", true),
        ],
    );
    f.view.selected = 1;
    let generation = f.view.generation;
    f.view
        .load_tx
        .send(LoadResult::Directory(
            generation,
            PathBuf::from("/project/a"),
            Ok(data::DirectoryListing {
                entries: vec![directory_entry("/project/a/file", false)],
                truncated: false,
            }),
        ))
        .unwrap();
    f.view.drain_loads();
    assert_eq!(
        f.view.selected_tree_row().unwrap().path,
        PathBuf::from("/project/z")
    );
    f.view.cancel_loads();
    f.view.clear_workspace();
    f.view
        .load_tx
        .send(LoadResult::Directory(
            generation,
            root,
            Ok(data::DirectoryListing {
                entries: vec![directory_entry("/project/WRONG", false)],
                truncated: false,
            }),
        ))
        .unwrap();
    f.view.drain_loads();
    assert!(f.view.items().is_empty());
}

#[tokio::test]
async fn change_mode_tabs_are_clickable_wrap_and_discard_stale_patches() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    f.view.workspace_verified = true;
    f.view.set_repositories(vec![repo("feature/tree")]);
    let mut buf = Buffer::empty(Rect::new(0, 0, 50, 24));
    f.view.render(buf.area, &mut buf);
    for label in [
        "1 Branch",
        "2 Unstaged",
        "3 Staged",
        "4 Untracked",
        "5 Turn patch",
    ] {
        assert!(buffer_text(&buf).contains(label));
    }
    assert!(
        f.view
            .change_tab_hit_areas
            .iter()
            .map(|(area, _)| area.y)
            .collect::<BTreeSet<_>>()
            .len()
            > 1
    );
    let (area, _) = *f
        .view
        .change_tab_hit_areas
        .iter()
        .find(|(_, kind)| *kind == ChangeKind::Staged)
        .unwrap();
    let old_generation = f.view.detail_generation;
    assert!(f.view.activate_tab(area.x, area.y));
    assert_eq!(f.view.change_kind, ChangeKind::Staged);
    f.view
        .load_tx
        .send(LoadResult::Change(
            old_generation,
            Ok("diff --git a/WRONG b/WRONG\n-old\n+new".into()),
        ))
        .unwrap();
    f.view.drain_loads();
    assert!(f.view.change_files.is_empty());
    f.view.render(buf.area, &mut buf);
    let (area, _) = *f
        .view
        .change_tab_hit_areas
        .iter()
        .find(|(_, kind)| *kind == ChangeKind::Staged)
        .unwrap();
    assert!(
        buf[(area.x, area.y)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
    f.view.select_category(Category::Files);
    f.view.render(buf.area, &mut buf);
    assert!(f.view.change_tab_hit_areas.is_empty());
}

#[tokio::test]
async fn clicking_file_tree_toggles_folder_with_scroll_offset() {
    let mut f = fixture().await;
    f.view.category = Category::Files;
    f.view.directories.insert(
        PathBuf::from("/project"),
        vec![
            directory_entry("/project/a", true),
            directory_entry("/project/b", true),
        ],
    );
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 20));
    f.view.render(buf.area, &mut buf);
    let area = f.view.list_hit_area;
    f.view.activate_list_row(area.x + 2, area.y + 1);
    assert_eq!(
        f.view.selected_tree_row().unwrap().path,
        PathBuf::from("/project/b")
    );
    assert!(f.view.expanded_files.contains(&PathBuf::from("/project/b")));
    f.view.scroll = 1;
    f.view.activate_list_row(area.x + 2, area.y);
    assert!(!f.view.expanded_files.contains(&PathBuf::from("/project/b")));
}

#[tokio::test]
async fn tree_keyboard_controls_keep_screen_switching_and_preview_focus_distinct() {
    let mut f = fixture().await;
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    f.view.category = Category::Files;
    f.view.directories.insert(
        PathBuf::from("/project"),
        vec![directory_entry("/project/src", true)],
    );
    f.view.directories.insert(
        PathBuf::from("/project/src"),
        vec![directory_entry("/project/src/main.rs", false)],
    );
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(f.view.focus, Focus::List);
    assert_eq!(f.view.items().len(), 2);
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(f.view.selected, 1);
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(f.view.focus, Focus::Preview);
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(f.view.preview_scroll, 1);
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(f.view.focus, Focus::List);
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(f.view.selected, 0);
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(f.view.focus, Focus::Preview);
    assert!(
        f.view
            .expanded_files
            .contains(&PathBuf::from("/project/src"))
    );
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(f.view.category, Category::Changes);
    f.view.handle_key(
        &mut tui,
        KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE),
    );
    assert_eq!(f.view.change_kind, ChangeKind::Untracked);
    f.view.handle_key(
        &mut tui,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    assert_eq!(f.view.change_kind, ChangeKind::TurnPatch);
    assert!(!f.view.is_done);
}

#[tokio::test]
async fn file_explorer_wide_and_narrow_layouts_use_relative_tree_paths() {
    let mut f = fixture().await;
    f.view.category = Category::Files;
    f.view.directories.insert(
        PathBuf::from("/project"),
        vec![
            directory_entry("/project/src", true),
            directory_entry("/project/README", false),
            directory_entry("/project/.config", true),
        ],
    );
    f.view.directories.insert(
        PathBuf::from("/project/src"),
        vec![
            directory_entry("/project/src/nested", true),
            directory_entry("/project/src/main.rs", false),
        ],
    );
    f.view
        .directories
        .insert(PathBuf::from("/project/src/nested"), Vec::new());
    f.view.expanded_files.insert(PathBuf::from("/project/src"));
    f.view.selected = 3;
    f.view.preview_path = Some(PathBuf::from("/project/src/main.rs"));
    f.view.file_text = Some("fn main() {\n    println!(\"Hello\");\n}\n".into());
    let mut wide = Buffer::empty(Rect::new(0, 0, 120, 20));
    f.view.render(wide.area, &mut wide);
    assert!(!buffer_text(&wide).contains("/project"));
    insta::assert_snapshot!("file_explorer_wide", buffer_text(&wide));
    let mut narrow = Buffer::empty(Rect::new(0, 0, 60, 20));
    f.view.render(narrow.area, &mut narrow);
    insta::assert_snapshot!("file_explorer_narrow", buffer_text(&narrow));
}

fn named_repo(name: &str) -> Repository {
    let mut value = repo("feature/tree");
    value.root = PathBuf::from(format!("/project/{name}"));
    value.identity = value.root.display().to_string();
    value
}

#[tokio::test]
async fn repository_tabs_hide_only_confirmed_clean_and_select_bookmarked_checkout() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    f.view.set_repositories(vec![
        named_repo("api"),
        named_repo("clean-lib"),
        named_repo("worker"),
        named_repo("unknown"),
        named_repo("error"),
    ]);
    f.view.preferences.set(
        f.view.bookmark_key("/project/worker"),
        RepoBookmark {
            target: Some("worker-target".into()),
            base: Some("worker-base".into()),
        },
    );
    f.view
        .repository_changes
        .insert("/project/api".into(), Ok(false));
    f.view
        .repository_changes
        .insert("/project/clean-lib".into(), Ok(false));
    f.view
        .repository_changes
        .insert("/project/worker".into(), Ok(true));
    f.view
        .repository_changes
        .insert("/project/error".into(), Err("Unavailable".into()));
    assert_eq!(f.view.visible_repositories(), vec![0, 2, 3, 4]);
    let mut buf = Buffer::empty(Rect::new(0, 0, 160, 24));
    f.view.render(buf.area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("api (clean)"));
    assert!(text.contains("worker *"));
    assert!(text.contains("unknown"));
    assert!(text.contains("error ?"));
    assert!(!text.contains("clean-lib"));
    let (area, _) = f
        .view
        .repository_tab_hit_areas
        .iter()
        .find(|(_, action)| *action == RepositoryTabAction::Select("/project/worker".into()))
        .unwrap()
        .clone();
    assert!(f.view.activate_tab(area.x, area.y));
    assert_eq!(
        f.view.selected_repository().unwrap().identity,
        "/project/worker"
    );
    assert_eq!(f.view.target_ref.as_deref(), Some("worker-target"));
    assert_eq!(f.view.base_ref.as_deref(), Some("worker-base"));
    assert_eq!(f.view.visible_repositories(), vec![2, 3, 4]);
    let (toggle, _) = f
        .view
        .repository_tab_hit_areas
        .iter()
        .find(|(_, action)| *action == RepositoryTabAction::ToggleClean)
        .unwrap()
        .clone();
    f.view.activate_tab(toggle.x, toggle.y);
    assert_eq!(f.view.visible_repositories(), vec![0, 1, 2, 3, 4]);
    assert!(f.view.show_clean_repositories);
    f.view.show_clean_repositories = false;
    f.view.cycle_repository(1);
    assert_eq!(
        f.view.selected_repository().unwrap().identity,
        "/project/unknown"
    );
}

#[tokio::test]
async fn repository_tab_pagination_is_bounded_clickable_and_unicode_safe() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    f.view.set_repositories(
        (0..20)
            .map(|index| named_repo(&format!("项目-service-{index:02}")))
            .collect(),
    );
    let mut buf = Buffer::empty(Rect::new(0, 0, 50, 22));
    f.view.render(buf.area, &mut buf);
    let pages = f.view.repository_tab_pages(50);
    assert!(pages.len() > 1);
    assert!(f.view.repository_tabs_height(50) <= 4);
    let (next, _) = f
        .view
        .repository_tab_hit_areas
        .iter()
        .find(|(_, action)| *action == RepositoryTabAction::Next)
        .unwrap()
        .clone();
    f.view.activate_tab(next.x, next.y);
    f.view.render(buf.area, &mut buf);
    assert_eq!(f.view.repository_tab_page, 1);
    let (tab, action) = f
        .view
        .repository_tab_hit_areas
        .iter()
        .find(|(_, action)| matches!(action, RepositoryTabAction::Select(_)))
        .unwrap()
        .clone();
    let RepositoryTabAction::Select(identity) = action else {
        panic!("a repository tab")
    };
    f.view.activate_tab(tab.x, tab.y);
    assert_eq!(f.view.selected_repository().unwrap().identity, identity);
    f.view.select_repository(19);
    f.view.render(buf.area, &mut buf);
    assert!(f.view.repository_tab_hit_areas.iter().any(
        |(_, action)| *action == RepositoryTabAction::Select("/project/项目-service-19".into())
    ));
    for (area, _) in &f.view.repository_tab_hit_areas {
        assert!(area.right() <= buf.area.right());
    }
    let generation = f.view.repository_check_generation;
    for index in 0..19 {
        f.view
            .load_tx
            .send(LoadResult::RepositoryChange(
                generation,
                format!("/project/项目-service-{index:02}"),
                Ok(false),
            ))
            .unwrap();
    }
    f.view.drain_loads();
    f.view.render(buf.area, &mut buf);
    assert_eq!(f.view.repository_tab_page, 0);
    assert!(f.view.repository_tab_hit_areas.iter().any(
        |(_, action)| *action == RepositoryTabAction::Select("/project/项目-service-19".into())
    ));
    f.view.select_change_kind(ChangeKind::TurnPatch);
    f.view.render(buf.area, &mut buf);
    assert!(f.view.repository_tab_hit_areas.is_empty());
    assert!(buffer_text(&buf).contains("Recorded tool edits across this chat"));
}

#[tokio::test]
async fn repository_probe_results_reject_old_modes_and_scopes() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    f.view
        .set_repositories(vec![named_repo("api"), named_repo("worker")]);
    let old_generation = f.view.repository_check_generation;
    f.view.select_change_kind(ChangeKind::Staged);
    let new_generation = f.view.repository_check_generation;
    f.view
        .load_tx
        .send(LoadResult::RepositoryChange(
            old_generation,
            "/project/worker".into(),
            Ok(false),
        ))
        .unwrap();
    f.view
        .load_tx
        .send(LoadResult::RepositoryChange(
            new_generation,
            "/project/worker".into(),
            Ok(true),
        ))
        .unwrap();
    f.view.drain_loads();
    assert_eq!(f.view.repository_changes["/project/worker"], Ok(true));
    f.view
        .load_tx
        .send(LoadResult::RepositoryChange(
            new_generation,
            "/project/worker".into(),
            Ok(false),
        ))
        .unwrap();
    f.view.cancel_loads();
    f.view.clear_workspace();
    f.view.drain_loads();
    assert!(f.view.repository_changes.is_empty());
}

#[derive(Default)]
struct AvailabilityRunner {
    changed: std::sync::atomic::AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}
impl crate::workspace_command::WorkspaceCommandExecutor for AvailabilityRunner {
    fn run(
        &self,
        command: crate::workspace_command::WorkspaceCommand,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        crate::workspace_command::WorkspaceCommandOutput,
                        crate::workspace_command::WorkspaceCommandError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let is_diff = command.argv.iter().any(|arg| arg == "diff");
            if is_diff {
                self.calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(crate::workspace_command::WorkspaceCommandOutput {
                exit_code: if is_diff {
                    i32::from(self.changed.load(std::sync::atomic::Ordering::Relaxed))
                } else {
                    1
                },
                stdout: String::new(),
                stderr: String::new(),
            })
        })
    }
}

#[tokio::test]
async fn periodic_repository_checks_reveal_new_changes_and_stop_when_leaving_changes() {
    let mut f = fixture().await;
    tokio::time::pause();
    let runner = std::sync::Arc::new(AvailabilityRunner::default());
    f.view.runner = Some(runner.clone());
    f.view.category = Category::Changes;
    f.view.change_kind = ChangeKind::Unstaged;
    f.view
        .set_repositories(vec![named_repo("api"), named_repo("worker")]);
    f.view.start_repository_checks();
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(runner.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    f.view.workspace_verified = true;
    f.view.start_repository_checks();
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    f.view.drain_loads();
    assert_eq!(f.view.visible_repositories(), vec![0]);
    runner
        .changed
        .store(true, std::sync::atomic::Ordering::Relaxed);
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    f.view.drain_loads();
    assert_eq!(f.view.visible_repositories(), vec![0, 1]);
    f.view.select_category(Category::Plans);
    let calls = runner.calls.load(std::sync::atomic::Ordering::Relaxed);
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        runner.calls.load(std::sync::atomic::Ordering::Relaxed),
        calls
    );
}

#[tokio::test]
async fn repository_tabs_filtered_and_all_layouts() {
    let mut f = fixture().await;
    f.view.category = Category::Changes;
    f.view.change_kind = ChangeKind::Unstaged;
    f.view.set_repositories(vec![
        named_repo("api"),
        named_repo("clean-lib"),
        named_repo("worker"),
        named_repo("docs"),
    ]);
    for (name, changed) in [
        ("api", true),
        ("clean-lib", false),
        ("worker", true),
        ("docs", false),
    ] {
        f.view
            .repository_changes
            .insert(format!("/project/{name}"), Ok(changed));
    }
    f.view.set_change_text(Ok("diff --git a/app.rs b/app.rs\nindex abc..def 100644\n--- a/app.rs\n+++ b/app.rs\n@@ -1 +1 @@\n-before\n+after\n".into()));
    let mut wide = Buffer::empty(Rect::new(0, 0, 140, 20));
    f.view.render(wide.area, &mut wide);
    insta::assert_snapshot!("repository_tabs_filtered", buffer_text(&wide));
    f.view.show_clean_repositories = true;
    let mut narrow = Buffer::empty(Rect::new(0, 0, 60, 22));
    f.view.render(narrow.area, &mut narrow);
    insta::assert_snapshot!("repository_tabs_all_narrow", buffer_text(&narrow));
}

#[tokio::test]
async fn double_clicking_an_item_focuses_preview_in_every_outputs_category() {
    for category in Category::ALL {
        let mut f = fixture().await;
        f.view.category = category;
        f.view.set_conversation(ConversationData {
            plans: vec![
                doc("plan-1", "Earlier plan", "Earlier contents"),
                doc("plan-2", "Design", "Plan contents"),
            ],
            agents: vec![
                Agent {
                    id: "child-1".into(),
                    title: "Earlier worker".into(),
                    status: "completed".into(),
                    summary: "Earlier contents".into(),
                },
                Agent {
                    id: "child-2".into(),
                    title: "Worker".into(),
                    status: "completed".into(),
                    summary: "Agent contents".into(),
                },
            ],
            ..Default::default()
        });
        f.view.directories.insert(
            PathBuf::from("/project"),
            vec![directory_entry("/project/file.rs", false)],
        );
        f.view.set_change_text(Ok("diff --git a/file.rs b/file.rs\n--- a/file.rs\n+++ b/file.rs\n@@ -1 +1 @@\n-old\n+new\n".into()));
        if matches!(category, Category::Plans | Category::Agents) {
            f.view.selected = 1;
            f.view.scroll = 1;
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        f.view.render(buf.area, &mut buf);
        let area = f.view.list_hit_area;
        f.view.activate_list_row(area.x + 3, area.y);
        assert_eq!(f.view.focus, Focus::List);
        f.view.activate_list_row(area.x + 3, area.y);
        assert_eq!(f.view.focus, Focus::Preview);
        if category == Category::Plans {
            assert!(f.view.preview().contains("Plan contents"));
        }
        if category == Category::Agents {
            assert!(f.view.preview().contains("Agent contents"));
        }
        f.view.render(buf.area, &mut buf);
        assert!(f.view.list_hit_area.is_empty());
        assert!(buffer_text(&buf).contains("Preview"));
    }
}

#[tokio::test]
async fn file_double_click_requires_same_file_position_and_short_interval() {
    let mut f = fixture().await;
    f.view.category = Category::Files;
    f.view.directories.insert(
        PathBuf::from("/project"),
        vec![
            directory_entry("/project/a.rs", false),
            directory_entry("/project/b.rs", false),
        ],
    );
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 20));
    f.view.render(buf.area, &mut buf);
    let area = f.view.list_hit_area;
    f.view.activate_list_row(area.x + 3, area.y);
    f.view.activate_list_row(area.x + 3, area.y + 1);
    assert_eq!(f.view.focus, Focus::List);
    if let Some((at, ..)) = &mut f.view.last_list_click {
        *at = std::time::Instant::now() - std::time::Duration::from_secs(1);
    }
    f.view.activate_list_row(area.x + 3, area.y + 1);
    assert_eq!(f.view.focus, Focus::List);
    // The same screen cell can now represent another file after a refresh.
    f.view.directories.insert(
        PathBuf::from("/project"),
        vec![
            directory_entry("/project/a.rs", false),
            directory_entry("/project/c.rs", false),
        ],
    );
    f.view.activate_list_row(area.x + 3, area.y + 1);
    assert_eq!(f.view.focus, Focus::List);
    f.view.activate_list_row(area.x + 6, area.y + 1);
    assert_eq!(f.view.focus, Focus::List);
    f.view.activate_list_row(area.x + 6, area.y + 1);
    assert_eq!(f.view.focus, Focus::Preview);
}

#[tokio::test]
async fn keyboard_navigation_and_folder_clicks_reset_file_double_clicks() {
    let mut f = fixture().await;
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    f.view.category = Category::Files;
    f.view.directories.insert(
        PathBuf::from("/project"),
        vec![
            directory_entry("/project/src", true),
            directory_entry("/project/file.rs", false),
        ],
    );
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 20));
    f.view.render(buf.area, &mut buf);
    let area = f.view.list_hit_area;
    f.view.activate_list_row(area.x + 3, area.y + 1);
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    f.view.activate_list_row(area.x + 3, area.y + 1);
    assert_eq!(f.view.focus, Focus::List);
    f.view.activate_list_row(area.x + 3, area.y);
    f.view.activate_list_row(area.x + 3, area.y);
    assert_eq!(f.view.focus, Focus::List);
    assert!(f.view.last_list_click.is_none());
    assert!(
        !f.view
            .expanded_files
            .contains(&PathBuf::from("/project/src"))
    );
}

#[tokio::test]
async fn mouse_wheel_scrolls_preview_without_changing_split_view_keyboard_focus() {
    let mut f = fixture().await;
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    f.view.set_conversation(ConversationData {
        plans: vec![
            doc("a", "First", &"content\n".repeat(100)),
            doc("b", "Second", "Another document"),
        ],
        ..Default::default()
    });
    f.view.load_selected_preview();
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 20));
    f.view.render(buf.area, &mut buf);
    f.view.scroll_preview_with_mouse(true);
    assert_eq!(f.view.preview_scroll, 3);
    assert_eq!(f.view.selected, 0);
    assert_eq!(f.view.focus, Focus::List);
    f.view.scroll_preview_with_mouse(false);
    assert_eq!(f.view.preview_scroll, 0);
    f.view.scroll_preview_with_mouse(true);
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(f.view.selected, 1);
    assert_eq!(f.view.preview_scroll, 0);
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    f.view
        .handle_key(&mut tui, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(f.view.selected, 1);
    assert_eq!(f.view.preview_scroll, 1);
    assert_eq!(f.view.focus, Focus::Preview);
}

#[tokio::test]
async fn mouse_wheel_reveals_narrow_preview_and_ignores_open_prompts() {
    let mut f = fixture().await;
    f.view.set_conversation(ConversationData {
        plans: vec![
            doc("a", "First", &"content\n".repeat(100)),
            doc("b", "Second", "Another document"),
        ],
        ..Default::default()
    });
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
    f.view.render(buf.area, &mut buf);
    f.view.prompt = Some(Prompt::Search);
    f.view.scroll_preview_with_mouse(true);
    assert_eq!(f.view.focus, Focus::List);
    assert_eq!(f.view.preview_scroll, 0);
    f.view.prompt = None;
    f.view.scroll_preview_with_mouse(true);
    assert_eq!(f.view.focus, Focus::Preview);
    assert_eq!(f.view.preview_scroll, 3);
    assert_eq!(f.view.selected, 0);
    f.view.render(buf.area, &mut buf);
    assert!(f.view.list_hit_area.is_empty());
    assert!(buffer_text(&buf).contains("content"));
}
