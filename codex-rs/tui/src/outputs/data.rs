//! Bounded, read-only loaders for the Outputs browser.
//!
//! All workspace access goes through [`WorkspaceCommandExecutor`].  In
//! particular, this module must not read the local TUI process filesystem: an
//! app-server can be running on a different host.

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SortDirection;
use codex_app_server_protocol::SubAgentActivityKind;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemsListCursor;
use codex_app_server_protocol::ThreadItemsListParams;
use codex_app_server_protocol::ThreadItemsListResponse;
use serde::Deserialize;
use uuid::Uuid;

use super::types::Agent;
use super::types::ChangeKind;
use super::types::ConversationData;
use super::types::Document;
use super::types::Repository;
use crate::workspace_command::WorkspaceCommand;
use crate::workspace_command::WorkspaceCommandExecutor;
use crate::workspace_command::WorkspaceCommandOutput;

const MAX_BRANCH_CREATIONS: usize = 128;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const DIFF_TIMEOUT: Duration = Duration::from_secs(20);
/// A UI can display a truncation notice when discovery reaches this bound.
pub(crate) const REPOSITORY_LIST_LIMIT: usize = 64;
/// A single directory is intentionally bounded so opening an unexpectedly
/// large workspace folder cannot overwhelm the TUI or app-server response.
pub(crate) const DIRECTORY_LIST_LIMIT: usize = 2_000;
const MAX_FILE_BYTES: usize = 512 * 1024;
const MAX_OUTPUT_CHARS: usize = 512 * 1024;
const MAX_UNTRACKED_FILES: usize = 50;
const MAX_ITEM_PAGES: usize = 4;
const ITEMS_PER_PAGE: u32 = 100;
const EXECUTABLE_FILTER_CONFIG_PATTERN: &str = r"^filter\..*\.(clean|process)$";

// These scripts are intentionally constants passed as a single argv element,
// not shell snippets.  They execute in the app-server workspace and return
// NUL-delimited paths so spaces and newlines are unambiguous.
const FIND_REPOSITORIES: &str = r#"
import os, sys
root = os.path.realpath(sys.argv[1])
limit, max_depth = int(sys.argv[2]), int(sys.argv[3])
skip = {'.git', 'node_modules', 'target', '.next', '.cache', 'vendor'}
found = []
visited = 0
for current, dirs, _ in os.walk(root, topdown=True, followlinks=False):
    visited += 1
    if visited > 5000:
        break
    rel = os.path.relpath(current, root)
    depth = 0 if rel == '.' else rel.count(os.sep) + 1
    dirs[:] = sorted(d for d in dirs if d not in skip and not d.startswith('.'))
    if depth > max_depth:
        dirs[:] = []
        continue
    marker = os.path.join(current, '.git')
    if os.path.isdir(marker) or os.path.isfile(marker):
        found.append(current)
        dirs[:] = []
        if len(found) >= limit: break
sys.stdout.buffer.write(b'\0'.join(os.fsencode(p) for p in found))
"#;

// This is an immediate-child listing, rather than a recursive search.  It is
// deliberately strict about symlinks: a path selected in the explorer must
// remain below the verified chat root without traversing a link on the way.
// JSON preserves filenames containing whitespace and newlines without relying
// on a delimiter shared with arbitrary file names.
const LIST_DIRECTORY: &str = r#"
import heapq, json, os, sys

raw_root = os.path.abspath(sys.argv[1])
directory = os.path.abspath(sys.argv[2])
limit = int(sys.argv[3])

def fail(message):
    sys.stderr.write(message)
    sys.exit(2)

try:
    if os.path.commonpath([raw_root, directory]) != raw_root:
        fail('path is outside the selected root')
    relative = os.path.relpath(directory, raw_root)
    current = raw_root
    if relative != '.':
        for component in relative.split(os.sep):
            current = os.path.join(current, component)
            if os.path.islink(current):
                fail('path contains a symlink')
    root = os.path.realpath(raw_root)
    resolved = os.path.realpath(directory)
    if os.path.commonpath([root, resolved]) != root:
        fail('path is outside the selected root')
    if not os.path.isdir(directory):
        fail('path is not a regular directory')

    def entries():
        with os.scandir(directory) as scan:
            for entry in scan:
                # A linked .git directory/file is metadata too; all other links
                # are skipped so they cannot later become navigation targets.
                if entry.name == '.git' or entry.is_symlink():
                    continue
                try:
                    if entry.is_dir(follow_symlinks=False):
                        yield {'path': entry.path, 'is_dir': True}
                    elif entry.is_file(follow_symlinks=False):
                        yield {'path': entry.path, 'is_dir': False}
                except OSError:
                    continue
    entries = heapq.nsmallest(
        limit + 1,
        entries(),
        key=lambda entry: (not entry['is_dir'], os.path.basename(entry['path'])),
    )
    truncated = len(entries) > limit
    sys.stdout.write(json.dumps({'entries': entries[:limit], 'truncated': truncated}, ensure_ascii=True))
except (OSError, ValueError) as error:
    fail(str(error))
"#;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct DirectoryEntry {
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct DirectoryListing {
    pub(crate) entries: Vec<DirectoryEntry>,
    pub(crate) truncated: bool,
}

const READ_FILE: &str = r#"
import os, sys
root, path, limit = os.path.realpath(sys.argv[1]), os.path.realpath(sys.argv[2]), int(sys.argv[3])
try:
    if os.path.commonpath([root, path]) != root: raise ValueError('path is outside the selected root')
    if not os.path.isfile(path): raise ValueError('path is not a regular file')
    if os.path.getsize(path) > limit: raise ValueError('file exceeds the viewer size limit')
    with open(path, 'rb') as f: data = f.read(limit + 1)
    if len(data) > limit: raise ValueError('file exceeds the viewer size limit')
    if b'\0' in data: raise ValueError('binary files cannot be displayed')
    sys.stdout.write(data.decode('utf-8'))
except (OSError, UnicodeDecodeError, ValueError) as e:
    sys.stderr.write(str(e)); sys.exit(2)
"#;

pub(crate) async fn discover_repositories(
    runner: &dyn WorkspaceCommandExecutor,
    cwd: &Path,
) -> Result<Vec<Repository>, String> {
    // A chat started below a checkout has exactly one relevant checkout.  This
    // avoids walking a parent directory when the intent is already explicit.
    if let Some(root) = git_stdout(runner, cwd, &["rev-parse", "--show-toplevel"]).await? {
        return Ok(vec![
            repository(runner, Path::new(root.strip_suffix('\n').unwrap_or(&root))).await?,
        ]);
    }

    let output = run(
        runner,
        python_command(
            FIND_REPOSITORIES,
            cwd,
            &[
                cwd,
                &PathBuf::from(REPOSITORY_LIST_LIMIT.to_string()),
                &PathBuf::from("5"),
            ],
        ),
    )
    .await?;
    if !output.success() {
        return Err(python_error("discover repositories", &output));
    }
    let mut roots = nul_paths(&output.stdout);
    roots.sort();
    roots.dedup();
    let mut repositories = Vec::new();
    for root in roots.into_iter().take(REPOSITORY_LIST_LIMIT) {
        if let Ok(repo) = repository(runner, &root).await {
            repositories.push(repo);
        }
    }
    repositories.sort_by(|a, b| a.root.cmp(&b.root));
    Ok(repositories)
}

/// Resolve the containing checkout on the workspace host, including cwd aliases.
pub(crate) async fn containing_checkout(
    runner: &dyn WorkspaceCommandExecutor,
    cwd: &Path,
) -> Result<Option<String>, String> {
    Ok(git_stdout(runner, cwd, &["rev-parse", "--show-toplevel"])
        .await?
        .map(|root| root.strip_suffix('\n').unwrap_or(&root).to_owned()))
}

/// Lists only the immediate children of `directory` below the verified chat
/// root.  Entries are directories-first then name-sorted, and symlinks and
/// Git metadata are never exposed as explorer targets.
pub(crate) async fn list_directory(
    runner: &dyn WorkspaceCommandExecutor,
    root: &Path,
    directory: &Path,
) -> Result<DirectoryListing, String> {
    let output = run(
        runner,
        python_command(
            LIST_DIRECTORY,
            root,
            &[
                root,
                directory,
                &PathBuf::from(DIRECTORY_LIST_LIMIT.to_string()),
            ],
        ),
    )
    .await?;
    if !output.success() {
        return Err(python_error("list directory", &output));
    }
    serde_json::from_str(&output.stdout)
        .map_err(|error| format!("Cannot parse directory listing from workspace: {error}"))
}

pub(crate) async fn read_file(
    runner: &dyn WorkspaceCommandExecutor,
    path: &Path,
    root: &Path,
) -> Result<String, String> {
    let output = run(
        runner,
        python_command(
            READ_FILE,
            root,
            &[root, path, &PathBuf::from(MAX_FILE_BYTES.to_string())],
        ),
    )
    .await?;
    if !output.success() {
        return Err(python_error("read file", &output));
    }
    Ok(truncate(&output.stdout, MAX_OUTPUT_CHARS))
}

pub(crate) async fn load_change(
    runner: &dyn WorkspaceCommandExecutor,
    repo: &Repository,
    kind: ChangeKind,
    target: &str,
    base: &str,
) -> Result<String, String> {
    if matches!(kind, ChangeKind::Working | ChangeKind::Staged) {
        ensure_working_branch_matches(runner, repo, target).await?;
    }
    let args: Vec<String> = match kind {
        ChangeKind::Working => {
            let args = diff_args(&["diff"]);
            let output = run(runner, safe_diff_command(runner, &repo.root, &args).await?).await?;
            ensure_working_branch_matches(runner, repo, target).await?;
            let tracked = if output.success() || output.exit_code == 1 {
                output.stdout
            } else {
                return Err(git_error(&args, &output));
            };
            let untracked = untracked_diff(runner, &repo.root).await?;
            ensure_working_branch_matches(runner, repo, target).await?;
            let separator =
                if !tracked.is_empty() && !tracked.ends_with('\n') && !untracked.is_empty() {
                    "\n"
                } else {
                    ""
                };
            return Ok(truncate(
                &format!("{tracked}{separator}{untracked}"),
                MAX_OUTPUT_CHARS,
            ));
        }
        ChangeKind::Staged => diff_args(&["diff", "--cached"]),
        ChangeKind::TurnPatch => return Err("Turn patches come from conversation history.".into()),
        ChangeKind::Branch => {
            let target = resolve_commit(runner, &repo.root, target).await?;
            let base = resolve_commit(runner, &repo.root, base).await?;
            let merge_base =
                git_required(runner, &repo.root, &["merge-base", &base, &target]).await?;
            let merge_base = full_oid(merge_base.trim())
                .ok_or_else(|| "Git returned an invalid merge-base.".to_string())?;
            diff_args(&["diff", &merge_base, &target])
        }
    };
    let output = run(runner, safe_diff_command(runner, &repo.root, &args).await?).await?;
    if matches!(kind, ChangeKind::Staged) {
        ensure_working_branch_matches(runner, repo, target).await?;
    }
    if output.success() || output.exit_code == 1 {
        Ok(truncate(&output.stdout, MAX_OUTPUT_CHARS))
    } else {
        Err(git_error(&args, &output))
    }
}

/// Returns whether a repository has changes for one of the repository-backed
/// Changes views.  This deliberately asks Git only for its exit status: it
/// must stay cheap enough to probe every discovered repository before drawing
/// the repository tabs.
///
/// `TurnPatch` belongs to the conversation rather than a repository and must
/// be handled by its caller.
pub(crate) async fn has_changes(
    runner: &dyn WorkspaceCommandExecutor,
    repo: &Repository,
    kind: ChangeKind,
    target: &str,
    base: &str,
) -> Result<bool, String> {
    if matches!(kind, ChangeKind::Working | ChangeKind::Staged) {
        ensure_working_branch_matches(runner, repo, target).await?;
    }
    let args: Vec<String> = match kind {
        ChangeKind::Working => {
            let args = quiet_diff_args(&["diff"]);
            let output = run(runner, safe_diff_command(runner, &repo.root, &args).await?).await?;
            ensure_working_branch_matches(runner, repo, target).await?;
            match output.exit_code {
                1 => return Ok(true),
                0 => {}
                _ => return Err(git_error(&args, &output)),
            }
            let files = git_required(
                runner,
                &repo.root,
                &["ls-files", "--others", "--exclude-standard", "-z"],
            )
            .await?;
            ensure_working_branch_matches(runner, repo, target).await?;
            return Ok(files.split('\0').any(|path| !path.is_empty()));
        }
        ChangeKind::Staged => quiet_diff_args(&["diff", "--cached"]),
        ChangeKind::TurnPatch => {
            return Err("Turn patches come from conversation history.".into());
        }
        ChangeKind::Branch => {
            let target = resolve_commit(runner, &repo.root, target).await?;
            let base = resolve_commit(runner, &repo.root, base).await?;
            let merge_base =
                git_required(runner, &repo.root, &["merge-base", &base, &target]).await?;
            let merge_base = full_oid(merge_base.trim())
                .ok_or_else(|| "Git returned an invalid merge-base.".to_string())?;
            quiet_diff_args(&["diff", &merge_base, &target])
        }
    };
    let output = run(runner, safe_diff_command(runner, &repo.root, &args).await?).await?;
    if matches!(kind, ChangeKind::Staged) {
        ensure_working_branch_matches(runner, repo, target).await?;
    }
    match output.exit_code {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(git_error(&args, &output)),
    }
}

/// A full local ref is provenance for a particular checkout. Working-tree
/// views must not silently render another branch after the user switches
/// branches or detaches HEAD. Short branch targets obey the same rule.
async fn ensure_working_branch_matches(
    runner: &dyn WorkspaceCommandExecutor,
    repo: &Repository,
    target: &str,
) -> Result<(), String> {
    if target.is_empty() || target == "HEAD" {
        return Ok(());
    }
    let expected = if target.starts_with("refs/") {
        target.to_owned()
    } else {
        format!("refs/heads/{target}")
    };
    let current = git_stdout(runner, &repo.root, &["symbolic-ref", "--quiet", "HEAD"]).await?;
    if current.as_deref().map(str::trim) == Some(expected.as_str()) {
        return Ok(());
    }
    if current.is_none()
        && full_oid(target).is_some()
        && git_required(runner, &repo.root, &["rev-parse", "HEAD"])
            .await?
            .trim()
            == target
    {
        return Ok(());
    }
    Err(format!(
        "Working tree changes are unavailable because this checkout is not on {target}."
    ))
}

/// Read metadata without attaching, resuming, or loading the thread's turns.
pub(crate) async fn load_workspace_metadata(
    handle: &AppServerRequestHandle,
    thread_id: &str,
) -> Result<codex_app_server_protocol::Thread, String> {
    let response: codex_app_server_protocol::ThreadReadResponse = tokio::time::timeout(
        Duration::from_secs(10),
        handle.request_typed(ClientRequest::ThreadRead {
            request_id: RequestId::String(Uuid::new_v4().to_string()),
            params: codex_app_server_protocol::ThreadReadParams {
                thread_id: thread_id.to_owned(),
                include_turns: false,
            },
        }),
    )
    .await
    .map_err(|_| "Workspace context request timed out.".to_owned())?
    .map_err(|e| e.to_string())?;
    Ok(response.thread)
}

pub(crate) async fn load_conversation(
    handle: &AppServerRequestHandle,
    thread_id: &str,
) -> Result<ConversationData, String> {
    let (items, partial) = list_items(handle, thread_id).await?;
    // A turn with no items must suppress the previous turn's recorded patch.
    let latest: codex_app_server_protocol::ThreadTurnsListResponse = handle
        .request_typed(ClientRequest::ThreadTurnsList {
            request_id: RequestId::String(Uuid::new_v4().to_string()),
            params: codex_app_server_protocol::ThreadTurnsListParams {
                thread_id: thread_id.into(),
                cursor: None,
                limit: Some(1),
                sort_direction: Some(SortDirection::Desc),
                items_view: Some(codex_app_server_protocol::TurnItemsView::NotLoaded),
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    let latest_turn = latest.data.first().map(|turn| turn.id.clone());
    let mut turn_patch = String::new();
    let mut data = ConversationData::default();
    let mut agent_status = BTreeMap::<String, Agent>::new();
    for entry in items {
        // Items are oldest-first (see `list_items`), so retaining this order
        // lets the eventual association persistence use later evidence as the
        // newest record for a repository.
        data.branch_creations.extend(
            crate::branch_association::creations_from_item(&entry.item)
                .into_iter()
                .map(|mut creation| {
                    creation.completed_at_ms = entry.completed_at_ms;
                    creation
                }),
        );
        if data.branch_creations.len() > MAX_BRANCH_CREATIONS {
            let excess = data.branch_creations.len() - MAX_BRANCH_CREATIONS;
            data.branch_creations.drain(..excess);
        }
        match entry.item {
            ThreadItem::Plan { id, text } => data.plans.push(Document {
                id,
                title: "Proposed plan".into(),
                markdown: text,
            }),
            ThreadItem::FileChange {
                changes,
                status: codex_app_server_protocol::PatchApplyStatus::Completed,
                ..
            } => {
                for change in changes {
                    if latest_turn.as_ref() == Some(&entry.turn_id)
                        && turn_patch.len() < MAX_OUTPUT_CHARS
                    {
                        turn_patch.push_str(&format!("diff -- {}\n{}\n", change.path, change.diff));
                    }
                    data.files.push(PathBuf::from(change.path));
                }
            }
            ThreadItem::SubAgentActivity {
                agent_thread_id,
                agent_path,
                kind,
                ..
            } => {
                let status = match kind {
                    SubAgentActivityKind::Started => "running",
                    SubAgentActivityKind::Completed => "completed",
                    SubAgentActivityKind::Interrupted => "interrupted",
                    SubAgentActivityKind::Interacted => "active",
                };
                agent_status.insert(
                    agent_thread_id.clone(),
                    Agent {
                        id: agent_thread_id,
                        title: agent_path,
                        status: status.into(),
                        summary: String::new(),
                    },
                );
            }
            ThreadItem::CollabAgentToolCall {
                receiver_thread_ids,
                prompt,
                agents_states,
                ..
            } => {
                for id in receiver_thread_ids {
                    let (status, summary) = agents_states
                        .get(&id)
                        .map(|state| {
                            (
                                format!("{:?}", state.status).to_lowercase(),
                                state.message.clone().unwrap_or_default(),
                            )
                        })
                        .unwrap_or_else(|| {
                            ("requested".into(), prompt.clone().unwrap_or_default())
                        });
                    let agent = agent_status.entry(id.clone()).or_insert(Agent {
                        id,
                        title: "Subagent".into(),
                        status: String::new(),
                        summary: String::new(),
                    });
                    agent.status = status;
                    if !summary.is_empty() {
                        agent.summary = summary;
                    }
                }
            }
            _ => {}
        }
    }
    if !turn_patch.is_empty() {
        data.turn_patch = Some(truncate(&turn_patch, MAX_OUTPUT_CHARS));
    }
    data.files.sort();
    data.files.dedup();
    let descendants_complete = refresh_descendants(handle, thread_id, &mut agent_status).await;
    data.agents = agent_status.into_values().collect();
    if !descendants_complete {
        data.notice = Some("Agent list may be partial: descendant discovery is unavailable or reached its page limit.".into());
    }
    if partial {
        data.notice = Some(format!(
            "Showing the latest {MAX_ITEM_PAGES} history pages. Refresh does not load older output."
        ));
    }
    Ok(data)
}

/// Distinguish a missing local branch from an unavailable or replaced checkout.
/// Only a verified repository and Git's exact missing-ref result permit cleanup.
pub(crate) async fn association_branch_exists(
    runner: &dyn WorkspaceCommandExecutor,
    association: &super::preferences::BranchAssociation,
) -> Result<bool, String> {
    if !association.branch_ref.starts_with("refs/heads/") {
        return Err("Recorded branch is not a local branch ref.".into());
    }
    let root = &association.repo_root;
    git_required(runner, root, &["check-ref-format", &association.branch_ref]).await?;
    let canonical = git_required(runner, root, &["rev-parse", "--show-toplevel"]).await?;
    if Path::new(canonical.strip_suffix('\n').unwrap_or(&canonical)) != root {
        return Err("Recorded checkout identity changed.".into());
    }
    let args = ["show-ref", "--verify", "--quiet", &association.branch_ref];
    let owned_args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let output = run(runner, git_command(root, &owned_args, COMMAND_TIMEOUT)).await?;
    match output.exit_code {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(git_error(&owned_args, &output)),
    }
}

/// Confirms a recorded branch-creation event still refers to a local branch
/// in its recorded checkout.  It never reads `HEAD`, because the user may
/// have changed checkouts since the command ran.
pub(crate) async fn verify_created_branch(
    runner: &dyn WorkspaceCommandExecutor,
    creation: &crate::branch_association::CreatedBranch,
) -> Result<super::preferences::BranchAssociation, String> {
    let branch_ref = format!("refs/heads/{}", creation.branch);
    git_required(runner, &creation.cwd, &["check-ref-format", &branch_ref]).await?;
    let revision = format!("{branch_ref}^{{commit}}");
    let commit = git_required(
        runner,
        &creation.cwd,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &revision,
        ],
    )
    .await?;
    if full_oid(commit.trim()).is_none() {
        return Err(format!(
            "Recorded branch {:?} is not a commit in its checkout.",
            creation.branch
        ));
    }
    let repo_root = git_required(runner, &creation.cwd, &["rev-parse", "--show-toplevel"]).await?;
    let repo_root = PathBuf::from(repo_root.strip_suffix('\n').unwrap_or(&repo_root));
    if repo_root.as_os_str().is_empty() {
        return Err("Git did not return a repository root for the recorded branch.".into());
    }
    Ok(super::preferences::BranchAssociation {
        repo_root,
        branch_ref,
        evidence_id: creation.evidence_id.clone(),
        completed_at_ms: creation.completed_at_ms,
    })
}

async fn refresh_descendants(
    handle: &AppServerRequestHandle,
    root: &str,
    agents: &mut BTreeMap<String, Agent>,
) -> bool {
    use codex_app_server_protocol::ThreadListParams;
    use codex_app_server_protocol::ThreadListResponse;
    use codex_app_server_protocol::ThreadSourceKind;
    let mut cursor = None;
    for _ in 0..4 {
        let response: Result<ThreadListResponse, _> = match tokio::time::timeout(
            Duration::from_secs(5),
            handle.request_typed(ClientRequest::ThreadList {
                request_id: RequestId::String(Uuid::new_v4().to_string()),
                params: ThreadListParams {
                    excluded_thread_ids: None,
                    originators: None,
                    cursor: cursor.clone(),
                    limit: Some(100),
                    sort_key: None,
                    sort_direction: None,
                    model_providers: Some(Vec::new()),
                    source_kinds: Some(vec![
                        ThreadSourceKind::SubAgent,
                        ThreadSourceKind::SubAgentThreadSpawn,
                        ThreadSourceKind::SubAgentOther,
                    ]),
                    archived: None,
                    section_id: None,
                    project_id: None,
                    parent_thread_id: None,
                    ancestor_thread_id: Some(root.to_owned()),
                    cwd: None,
                    use_state_db_only: true,
                    search_term: None,
                },
            }),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => return false,
        };
        let Ok(response) = response else {
            return false;
        };
        for thread in response.data {
            if thread.id == root {
                continue;
            }
            let title = thread
                .agent_nickname
                .or(thread.name)
                .or(thread.agent_role)
                .unwrap_or_else(|| thread.id.clone());
            let agent = agents.entry(thread.id.clone()).or_insert(Agent {
                id: thread.id,
                title: title.clone(),
                status: String::new(),
                summary: String::new(),
            });
            agent.title = title;
            agent.status = agent_status_label(&thread.status).into();
            if agent.summary.is_empty() {
                agent.summary = thread.preview;
            }
        }
        match response.next_cursor {
            Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
            None => return true,
            _ => return false,
        }
    }
    false
}

fn agent_status_label(status: &codex_app_server_protocol::ThreadStatus) -> &'static str {
    use codex_app_server_protocol::ThreadActiveFlag;
    use codex_app_server_protocol::ThreadStatus;
    match status {
        ThreadStatus::Active { active_flags }
            if active_flags.contains(&ThreadActiveFlag::WaitingOnApproval) =>
        {
            "waiting on approval"
        }
        ThreadStatus::Active { active_flags }
            if active_flags.contains(&ThreadActiveFlag::WaitingOnUserInput) =>
        {
            "waiting on user input"
        }
        ThreadStatus::Active { .. } => "working",
        ThreadStatus::Idle => "ready",
        ThreadStatus::NotLoaded => "not loaded",
        ThreadStatus::SystemError => "error",
    }
}

pub(crate) async fn load_agent(
    handle: &AppServerRequestHandle,
    thread_id: &str,
) -> Result<String, String> {
    let (items, partial) = list_items(handle, thread_id).await?;
    let mut text = String::new();
    for entry in items {
        match entry.item {
            // Deliberately exclude ThreadItem::Reasoning: raw reasoning is not
            // an Outputs surface, even when old history still contains it.
            ThreadItem::AgentMessage { text: message, .. } => {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&message);
            }
            ThreadItem::Plan { text: plan, .. } => {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str("## Plan\n\n");
                text.push_str(&plan);
            }
            _ => {}
        }
        if text.len() >= MAX_OUTPUT_CHARS {
            break;
        }
    }
    let mut text = truncate(&text, MAX_OUTPUT_CHARS);
    if partial {
        text.push_str("\n\n[Only the latest bounded history pages are shown]");
    }
    Ok(text)
}

async fn list_items(
    handle: &AppServerRequestHandle,
    thread_id: &str,
) -> Result<(Vec<codex_app_server_protocol::ThreadItemEntry>, bool), String> {
    let mut items = Vec::new();
    let mut cursor = None;
    for _ in 0..MAX_ITEM_PAGES {
        let response: ThreadItemsListResponse = handle
            .request_typed(ClientRequest::ThreadItemsList {
                request_id: RequestId::String(Uuid::new_v4().to_string()),
                params: ThreadItemsListParams {
                    thread_id: thread_id.to_owned(),
                    turn_id: None,
                    cursor: cursor.map(ThreadItemsListCursor::Opaque),
                    limit: Some(ITEMS_PER_PAGE),
                    sort_direction: Some(SortDirection::Desc),
                },
            })
            .await
            .map_err(|err| err.to_string())?;
        items.extend(response.data);
        match response.next_cursor {
            Some(next) => cursor = Some(next),
            None => {
                items.reverse();
                return Ok((items, false));
            }
        }
    }
    // The caller can explicitly refresh; a bounded partial answer is safer
    // than hydrating arbitrary historical transcript data in the TUI.
    items.reverse();
    Ok((items, true))
}

pub(crate) async fn repository(
    runner: &dyn WorkspaceCommandExecutor,
    root: &Path,
) -> Result<Repository, String> {
    let root = git_required(runner, root, &["rev-parse", "--show-toplevel"]).await?;
    let root = PathBuf::from(root.strip_suffix('\n').unwrap_or(&root));
    let branch = git_stdout(
        runner,
        &root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .await?
    .map(|s| s.trim().to_owned());
    let branches = git_required(
        runner,
        &root,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .await?
    .lines()
    .filter(|line| !line.ends_with("/HEAD"))
    .map(str::to_owned)
    .collect();
    let default_base = default_base(runner, &root).await?;
    let head = resolve_commit(runner, &root, "HEAD").await.ok();
    Ok(Repository {
        identity: root.to_string_lossy().into_owned(),
        root,
        branch,
        head,
        branches,
        default_base,
    })
}

async fn default_base(
    runner: &dyn WorkspaceCommandExecutor,
    root: &Path,
) -> Result<Option<String>, String> {
    if let Some(origin) = git_stdout(
        runner,
        root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await?
        && resolve_commit(runner, root, origin.trim()).await.is_ok()
    {
        return Ok(Some(origin.trim().to_owned()));
    }
    for candidate in ["main", "master"] {
        if resolve_commit(runner, root, candidate).await.is_ok() {
            return Ok(Some(candidate.into()));
        }
    }
    Ok(None)
}

async fn resolve_commit(
    runner: &dyn WorkspaceCommandExecutor,
    root: &Path,
    reference: &str,
) -> Result<String, String> {
    let revision = format!("{reference}^{{commit}}");
    let value = git_required(
        runner,
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &revision,
        ],
    )
    .await?;
    full_oid(value.trim())
        .ok_or_else(|| format!("{reference:?} is not a commit available in this repository."))
}

async fn untracked_diff(
    runner: &dyn WorkspaceCommandExecutor,
    root: &Path,
) -> Result<String, String> {
    let files = git_required(
        runner,
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )
    .await?;
    let mut result = String::new();
    let paths = files
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    for path in paths.iter().take(MAX_UNTRACKED_FILES) {
        let args = diff_args(&["diff", "--no-index", "--", null_device(), path]);
        let output = run(runner, safe_diff_command(runner, root, &args).await?).await?;
        if output.success() || output.exit_code == 1 {
            if output.stdout.is_empty() {
                result.push_str(&format!("Untracked empty file: {path:?}\n"));
            } else {
                result.push_str(&output.stdout);
            }
        } else {
            return Err(git_error(&args, &output));
        }
        if result.len() >= MAX_OUTPUT_CHARS {
            break;
        }
    }
    let mut result = truncate(&result, MAX_OUTPUT_CHARS);
    if paths.len() > MAX_UNTRACKED_FILES {
        result.push_str(&format!(
            "\n\n[Only the first {MAX_UNTRACKED_FILES} untracked files are shown]"
        ));
    }
    Ok(result)
}

fn diff_args(prefix: &[&str]) -> Vec<String> {
    let mut args = vec![prefix[0].to_owned()];
    args.extend(
        [
            "--no-textconv",
            "--no-ext-diff",
            "--submodule=short",
            "--ignore-submodules=dirty",
            "--color=never",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    args.extend(prefix[1..].iter().map(|s| (*s).to_owned()));
    args
}

fn quiet_diff_args(prefix: &[&str]) -> Vec<String> {
    let mut args = diff_args(prefix);
    args.insert(1, "--quiet".into());
    args
}

fn git_command(root: &Path, args: &[String], timeout: Duration) -> WorkspaceCommand {
    let mut argv = vec![
        "git".into(),
        "-c".into(),
        codex_git_utils::SAFE_BARE_REPOSITORY_CONFIG.into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        if cfg!(windows) {
            "core.hooksPath=NUL".into()
        } else {
            "core.hooksPath=/dev/null".into()
        },
    ];
    argv.extend(args.iter().cloned());
    WorkspaceCommand::new(argv, root.to_path_buf())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .timeout(timeout)
}

/// Disable every configured clean/process filter while rendering a diff.  The
/// filter's executable is repository-controlled; `--no-textconv` alone does
/// not make arbitrary filter configuration a safe dependency.
async fn safe_diff_command(
    runner: &dyn WorkspaceCommandExecutor,
    root: &Path,
    args: &[String],
) -> Result<WorkspaceCommand, String> {
    let config_args = [
        "config",
        "--null",
        "--name-only",
        "--get-regexp",
        EXECUTABLE_FILTER_CONFIG_PATTERN,
    ];
    let output = run(
        runner,
        git_command(
            root,
            &config_args
                .iter()
                .map(|arg| (*arg).to_owned())
                .collect::<Vec<_>>(),
            COMMAND_TIMEOUT,
        ),
    )
    .await?;
    if output.exit_code != 0 && output.exit_code != 1 {
        return Err(git_error(
            &config_args
                .iter()
                .map(|arg| (*arg).to_owned())
                .collect::<Vec<_>>(),
            &output,
        ));
    }
    let mut drivers = output
        .stdout
        .split('\0')
        .filter_map(|key| {
            key.strip_suffix(".clean")
                .or_else(|| key.strip_suffix(".process"))
        })
        .collect::<Vec<_>>();
    drivers.sort_unstable();
    drivers.dedup();
    let mut command = git_command(root, args, DIFF_TIMEOUT);
    if !drivers.is_empty() {
        command = command.env("GIT_CONFIG_COUNT", (drivers.len() * 3).to_string());
        for (index, (key, value)) in drivers
            .into_iter()
            .flat_map(|driver| {
                [
                    (format!("{driver}.clean"), String::new()),
                    (format!("{driver}.process"), String::new()),
                    (format!("{driver}.required"), "false".to_owned()),
                ]
            })
            .enumerate()
        {
            command = command
                .env(format!("GIT_CONFIG_KEY_{index}"), key)
                .env(format!("GIT_CONFIG_VALUE_{index}"), value);
        }
    }
    Ok(command)
}

fn python_command(script: &str, cwd: &Path, paths: &[&Path]) -> WorkspaceCommand {
    // Workspace contents and Python startup hooks must not execute while browsing.
    let mut argv = vec![
        "python3".to_owned(),
        "-I".to_owned(),
        "-S".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
    ];
    argv.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
    WorkspaceCommand::new(argv, cwd.to_path_buf())
        .timeout(COMMAND_TIMEOUT)
        .disable_output_cap()
}

async fn git_stdout(
    runner: &dyn WorkspaceCommandExecutor,
    root: &Path,
    args: &[&str],
) -> Result<Option<String>, String> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let output = run(runner, git_command(root, &args, COMMAND_TIMEOUT)).await?;
    if output.success() {
        Ok(Some(output.stdout))
    } else if output.exit_code == 1 || output.exit_code == 128 {
        Ok(None)
    } else {
        Err(git_error(&args, &output))
    }
}

async fn git_required(
    runner: &dyn WorkspaceCommandExecutor,
    root: &Path,
    args: &[&str],
) -> Result<String, String> {
    git_stdout(runner, root, args)
        .await?
        .ok_or_else(|| format!("git {args:?} did not return a value"))
}

async fn run(
    runner: &dyn WorkspaceCommandExecutor,
    command: WorkspaceCommand,
) -> Result<WorkspaceCommandOutput, String> {
    runner.run(command).await.map_err(|err| err.to_string())
}

fn nul_paths(output: &str) -> Vec<PathBuf> {
    output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}
fn full_oid(value: &str) -> Option<String> {
    ((value.len() == 40 || value.len() == 64) && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_owned())
}
fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}
fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        value.to_owned()
    } else {
        let mut end = limit;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}\n\n[Output truncated at {} KiB]",
            &value[..end],
            limit / 1024
        )
    }
}
fn git_error(args: &[String], output: &WorkspaceCommandOutput) -> String {
    format!(
        "git {:?} failed ({}): {}",
        args,
        output.exit_code,
        output.stderr.trim()
    )
}
fn python_error(action: &str, output: &WorkspaceCommandOutput) -> String {
    format!(
        "Cannot {action} in this workspace (python3 is required): {}",
        output.stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_command::WorkspaceCommandError;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::process::Command as ProcessCommand;
    use std::sync::Mutex;

    struct FakeRunner {
        responses: Mutex<VecDeque<WorkspaceCommandOutput>>,
        commands: Mutex<Vec<WorkspaceCommand>>,
    }

    impl FakeRunner {
        fn new(responses: Vec<WorkspaceCommandOutput>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                commands: Mutex::new(Vec::new()),
            }
        }
    }

    impl WorkspaceCommandExecutor for FakeRunner {
        fn run(
            &self,
            command: WorkspaceCommand,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<WorkspaceCommandOutput, WorkspaceCommandError>>
                    + Send
                    + '_,
            >,
        > {
            self.commands.lock().unwrap().push(command);
            let response = self.responses.lock().unwrap().pop_front().unwrap();
            Box::pin(async move { Ok(response) })
        }
    }

    fn output(exit_code: i32, stdout: impl Into<String>) -> WorkspaceCommandOutput {
        WorkspaceCommandOutput {
            exit_code,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// A test-only executor which proves the argv-only request shape against
    /// real Git and Python, while still exercising the production loader.
    struct ProcessRunner;

    impl WorkspaceCommandExecutor for ProcessRunner {
        fn run(
            &self,
            command: WorkspaceCommand,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<WorkspaceCommandOutput, WorkspaceCommandError>>
                    + Send
                    + '_,
            >,
        > {
            let mut process = ProcessCommand::new(&command.argv[0]);
            process.args(&command.argv[1..]).current_dir(command.cwd);
            for (key, value) in command.env {
                match value {
                    Some(value) => {
                        process.env(key, value);
                    }
                    None => {
                        process.env_remove(key);
                    }
                }
            }
            let output = process.output().expect("fixture command should launch");
            Box::pin(async move {
                Ok(WorkspaceCommandOutput {
                    exit_code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                })
            })
        }
    }

    fn git(cwd: &Path, args: &[&str]) {
        let status = ProcessCommand::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git fixture command should launch");
        assert!(status.success(), "git {args:?} failed");
    }

    fn repository_fixture(parent: &Path, name: &str) -> PathBuf {
        let root = parent.join(name);
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "test@example.invalid"]);
        git(&root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        git(&root, &["add", "tracked.txt"]);
        git(&root, &["commit", "-qm", "initial"]);
        root
    }

    #[tokio::test]
    async fn confirms_deleted_branches_without_confusing_unavailable_checkouts() {
        let temp = tempfile::tempdir().unwrap();
        let root = repository_fixture(temp.path(), "repo")
            .canonicalize()
            .unwrap();
        git(&root, &["branch", "owned"]);
        let association = super::super::preferences::BranchAssociation {
            repo_root: root.clone(),
            branch_ref: "refs/heads/owned".into(),
            evidence_id: "creation".into(),
            completed_at_ms: Some(1),
        };
        assert!(
            association_branch_exists(&ProcessRunner, &association)
                .await
                .unwrap()
        );
        let path = temp.path().join("thread.json");
        crate::branch_association::persist_associations(path.clone(), vec![association.clone()])
            .await
            .unwrap();
        git(&root, &["branch", "-d", "owned"]);
        assert!(
            !association_branch_exists(&ProcessRunner, &association)
                .await
                .unwrap()
        );
        let removed = crate::branch_association::prune_deleted_associations(
            std::sync::Arc::new(ProcessRunner),
            Some(path.clone()),
            vec![association.clone()],
        )
        .await
        .unwrap();
        assert_eq!(removed, vec![association.clone()]);
        assert!(
            crate::outputs::Preferences::load_with_associations(&path)
                .associations()
                .next()
                .is_none()
        );
        // A delayed writer must not bring the old link back.
        crate::branch_association::persist_associations(path.clone(), vec![association.clone()])
            .await
            .unwrap();
        assert!(
            crate::outputs::Preferences::load_with_associations(&path)
                .associations()
                .next()
                .is_none()
        );
        // An unreachable/corrupt checkout is not proof of deletion.
        std::fs::rename(root.join(".git"), root.join("metadata-unavailable")).unwrap();
        assert!(
            association_branch_exists(&ProcessRunner, &association)
                .await
                .is_err()
        );
        let broken_ref = FakeRunner::new(vec![
            output(0, ""),
            output(0, format!("{}\n", root.display())),
            output(128, ""),
        ]);
        assert!(
            association_branch_exists(&broken_ref, &association)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn verifies_recorded_branch_without_reading_current_head() {
        let temp = tempfile::tempdir().unwrap();
        let root = repository_fixture(temp.path(), "repo trailing ");
        let original_branch = String::from_utf8(
            ProcessCommand::new("git")
                .args(["branch", "--show-current"])
                .current_dir(&root)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        git(&root, &["branch", "recorded"]);
        git(&root, &["switch", "-q", "-c", "different-checkout"]);
        let creation = crate::branch_association::CreatedBranch {
            cwd: root.clone(),
            branch: "recorded".into(),
            evidence_id: "command-1".into(),
            completed_at_ms: None,
        };
        let verified = verify_created_branch(&ProcessRunner, &creation)
            .await
            .unwrap();
        assert_eq!(verified.repo_root, root.canonicalize().unwrap());
        assert_eq!(verified.branch_ref, "refs/heads/recorded");
        assert_eq!(verified.evidence_id, "command-1");

        let mut revision_expression = creation.clone();
        revision_expression.branch = "recorded^{commit}".into();
        assert!(
            verify_created_branch(&ProcessRunner, &revision_expression)
                .await
                .is_err()
        );

        git(&root, &["switch", "-q", &original_branch]);
        git(&root, &["branch", "-D", "recorded"]);
        assert!(
            verify_created_branch(&ProcessRunner, &creation)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn working_changes_require_the_recorded_branch_to_remain_checked_out() {
        let temp = tempfile::tempdir().unwrap();
        let root = repository_fixture(temp.path(), "repo");
        let original_branch = String::from_utf8(
            ProcessCommand::new("git")
                .args(["branch", "--show-current"])
                .current_dir(&root)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        git(&root, &["switch", "-q", "-c", "recorded"]);
        std::fs::write(root.join("tracked.txt"), "changed\n").unwrap();
        let repo = Repository {
            root: root.clone(),
            identity: root.to_string_lossy().into_owned(),
            branch: Some("recorded".into()),
            head: None,
            branches: vec![],
            default_base: None,
        };
        assert!(
            has_changes(
                &ProcessRunner,
                &repo,
                ChangeKind::Working,
                "refs/heads/recorded",
                ""
            )
            .await
            .unwrap()
        );
        git(&root, &["switch", "-q", &original_branch]);
        let error = has_changes(
            &ProcessRunner,
            &repo,
            ChangeKind::Working,
            "refs/heads/recorded",
            "",
        )
        .await
        .unwrap_err();
        assert!(error.contains("not on refs/heads/recorded"));
        for target in ["recorded", "refs/heads/recorded"] {
            for kind in [ChangeKind::Working, ChangeKind::Staged] {
                assert!(
                    load_change(&ProcessRunner, &repo, kind, target, "")
                        .await
                        .is_err()
                );
                assert!(
                    has_changes(&ProcessRunner, &repo, kind, target, "")
                        .await
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn full_oid_rejects_refs() {
        assert!(full_oid("a".repeat(40).as_str()).is_some());
        assert!(full_oid("main").is_none());
    }
    #[test]
    fn truncation_is_utf8_safe() {
        assert!(truncate("é", 1).contains("truncated"));
    }
    #[test]
    fn diff_does_not_enable_external_helpers() {
        let args = diff_args(&["diff"]);
        assert!(args.contains(&"--no-ext-diff".into()));
        assert!(args.contains(&"--no-textconv".into()));
    }

    #[tokio::test]
    async fn change_probe_checks_each_mode_without_running_configured_filters() {
        let temp = tempfile::tempdir().unwrap();
        let root = repository_fixture(temp.path(), "repo");
        let runner = ProcessRunner;
        let current_branch = String::from_utf8(
            ProcessCommand::new("git")
                .args(["branch", "--show-current"])
                .current_dir(&root)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        git(&root, &["checkout", "-qb", "other"]);
        std::fs::write(root.join("other.txt"), "branch-only\n").unwrap();
        git(&root, &["add", "other.txt"]);
        git(&root, &["commit", "-qm", "other"]);
        git(&root, &["checkout", "-q", &current_branch]);

        // This filter would make an ordinary worktree-to-index comparison
        // fail. The probe must inherit safe_diff_command's config overrides.
        std::fs::write(root.join(".gitattributes"), "tracked.txt filter=trip\n").unwrap();
        git(&root, &["add", ".gitattributes"]);
        git(&root, &["commit", "-qm", "attributes"]);
        git(&root, &["config", "filter.trip.clean", "false"]);

        let repo = repository(&runner, &root).await.unwrap();
        assert!(
            !has_changes(
                &runner,
                &repo,
                ChangeKind::Branch,
                &current_branch,
                &current_branch,
            )
            .await
            .unwrap()
        );
        assert!(
            has_changes(&runner, &repo, ChangeKind::Branch, "other", &current_branch,)
                .await
                .unwrap()
        );
        assert!(
            !has_changes(&runner, &repo, ChangeKind::Working, "", "")
                .await
                .unwrap()
        );

        std::fs::write(root.join("tracked.txt"), "unstaged\n").unwrap();
        assert!(
            has_changes(&runner, &repo, ChangeKind::Working, "", "")
                .await
                .unwrap()
        );
        assert!(
            !has_changes(&runner, &repo, ChangeKind::Staged, "", "")
                .await
                .unwrap()
        );

        git(&root, &["config", "--unset", "filter.trip.clean"]);
        git(&root, &["add", "tracked.txt"]);
        assert!(
            !has_changes(&runner, &repo, ChangeKind::Working, "", "")
                .await
                .unwrap()
        );
        assert!(
            has_changes(&runner, &repo, ChangeKind::Staged, "", "")
                .await
                .unwrap()
        );

        std::fs::write(root.join("untracked.txt"), "untracked\n").unwrap();
        assert!(
            has_changes(&runner, &repo, ChangeKind::Working, "", "")
                .await
                .unwrap()
        );
        assert!(
            has_changes(
                &runner,
                &repo,
                ChangeKind::Branch,
                "missing",
                &current_branch
            )
            .await
            .is_err()
        );
    }
    #[test]
    fn nul_paths_preserve_spaces() {
        assert_eq!(
            nul_paths("/a b\0/c"),
            vec![PathBuf::from("/a b"), PathBuf::from("/c")]
        );
    }

    #[tokio::test]
    async fn branch_comparison_resolves_refs_to_oids_without_checkout() {
        let target = "a".repeat(40);
        let base = "b".repeat(40);
        let merge_base = "c".repeat(40);
        let runner = FakeRunner::new(vec![
            output(0, format!("{target}\n")),
            output(0, format!("{base}\n")),
            output(0, format!("{merge_base}\n")),
            output(1, ""),
            output(1, "diff --git a/file b/file\n"),
        ]);
        let repo = Repository {
            root: PathBuf::from("/workspace/repo"),
            identity: "/workspace/repo".into(),
            branch: Some("feature".into()),
            head: Some(target.clone()),
            branches: vec!["feature".into(), "main".into()],
            default_base: Some("main".into()),
        };

        let diff = load_change(&runner, &repo, ChangeKind::Branch, "feature", "main")
            .await
            .unwrap();

        assert!(diff.starts_with("diff --git"));
        let commands = runner.commands.lock().unwrap();
        assert!(
            commands
                .iter()
                .all(|command| !command.argv.contains(&"checkout".into()))
        );
        assert!(commands.last().unwrap().argv.contains(&merge_base));
        assert!(commands.last().unwrap().argv.contains(&target));
    }

    #[tokio::test]
    async fn discovery_scopes_nested_checkout_and_keeps_linked_worktrees_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let first = repository_fixture(temp.path(), "first")
            .canonicalize()
            .unwrap();
        let second = repository_fixture(temp.path(), "second")
            .canonicalize()
            .unwrap();
        let runner = ProcessRunner;

        let nested_cwd = first.join("src");
        std::fs::create_dir(&nested_cwd).unwrap();
        let nested = discover_repositories(&runner, &nested_cwd).await.unwrap();
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].root, first);

        git(&first, &["worktree", "add", "-qb", "linked", "../linked"]);
        let repositories = discover_repositories(&runner, temp.path()).await.unwrap();
        assert!(repositories.iter().any(|repo| repo.root == first));
        assert!(repositories.iter().any(|repo| repo.root == second));
        let linked = temp.path().join("linked").canonicalize().unwrap();
        let linked_repo = repositories
            .iter()
            .find(|repo| repo.root == linked)
            .unwrap();
        let first_repo = repositories.iter().find(|repo| repo.root == first).unwrap();
        assert_ne!(first_repo.identity, linked_repo.identity);
        assert_ne!(first_repo.head, None);
    }

    #[tokio::test]
    async fn real_git_modes_do_not_checkout_and_file_reader_rejects_unsafe_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = repository_fixture(temp.path(), "repo");
        let runner = ProcessRunner;
        let main = ProcessCommand::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&root)
            .output()
            .unwrap();
        let main = String::from_utf8(main.stdout).unwrap().trim().to_owned();
        git(&root, &["checkout", "-qb", "other"]);
        std::fs::write(root.join("other.txt"), "branch-only\n").unwrap();
        git(&root, &["add", "other.txt"]);
        git(&root, &["commit", "-qm", "other"]);
        git(&root, &["checkout", "-q", &main]);
        std::fs::write(root.join("tracked.txt"), "unstaged\n").unwrap();
        std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
        git(&root, &["add", "staged.txt"]);
        std::fs::write(root.join("new file\nname.md"), "viewer\n").unwrap();
        std::fs::write(root.join("empty.txt"), "").unwrap();
        let repo = repository(&runner, &root).await.unwrap();
        let head_before = ProcessCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root)
            .output()
            .unwrap();
        let branch = load_change(&runner, &repo, ChangeKind::Branch, "other", &main)
            .await
            .unwrap();
        assert!(branch.contains("other.txt"));
        let working = load_change(&runner, &repo, ChangeKind::Working, "", "")
            .await
            .unwrap();
        // The Changes view presents both tracked worktree edits and new
        // files in one diff, while leaving staged changes to their own view.
        assert!(working.contains("unstaged"));
        assert!(working.contains("empty.txt"));
        assert!(working.contains("new file"));
        let head_after = ProcessCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert_eq!(head_before.stdout, head_after.stdout);
        assert!(
            load_change(&runner, &repo, ChangeKind::Staged, "", "")
                .await
                .unwrap()
                .contains("staged")
        );

        assert_eq!(
            read_file(&runner, &root.join("new file\nname.md"), &root)
                .await
                .unwrap(),
            "viewer\n"
        );
        std::fs::write(root.join("binary.txt"), [0, 1, 2]).unwrap();
        assert!(
            read_file(&runner, &root.join("binary.txt"), &root)
                .await
                .is_err()
        );
        std::fs::write(root.join("large.txt"), vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
        assert!(
            read_file(&runner, &root.join("large.txt"), &root)
                .await
                .is_err()
        );
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, "nope").unwrap();
        assert!(read_file(&runner, &outside, &root).await.is_err());
    }

    #[tokio::test]
    async fn directory_listing_is_immediate_and_includes_all_regular_file_kinds() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join(".hidden-dir")).unwrap();
        std::fs::create_dir(root.join("empty folder")).unwrap();
        std::fs::create_dir(root.join("target")).unwrap();
        std::fs::write(root.join("README"), "extensionless\n").unwrap();
        std::fs::write(root.join(".hidden"), "hidden\n").unwrap();
        std::fs::write(root.join("binary"), [0, 1, 2]).unwrap();
        std::fs::write(root.join("large"), vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
        std::fs::write(root.join("escaped name\nwith spaces.md"), "escaped\n").unwrap();
        std::fs::write(root.join(".git"), "gitdir: elsewhere\n").unwrap();
        let runner = ProcessRunner;

        let listing = list_directory(&runner, &root, &root).await.unwrap();
        let names = listing
            .entries
            .iter()
            .map(|entry| {
                (
                    entry
                        .path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    entry.is_dir,
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                (".hidden-dir".into(), true),
                ("empty folder".into(), true),
                ("target".into(), true),
                (".hidden".into(), false),
                ("README".into(), false),
                ("binary".into(), false),
                ("escaped name\nwith spaces.md".into(), false),
                ("large".into(), false),
            ]
        );
        assert!(!listing.truncated);
        assert_eq!(
            list_directory(&runner, &root, &root.join("empty folder"))
                .await
                .unwrap(),
            DirectoryListing {
                entries: Vec::new(),
                truncated: false,
            }
        );
    }

    #[tokio::test]
    async fn directory_listing_rejects_escape_and_symlink_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret"), "nope\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("linked-outside")).unwrap();
        let runner = ProcessRunner;

        assert!(list_directory(&runner, &root, &outside).await.is_err());
        #[cfg(unix)]
        {
            let listing = list_directory(&runner, &root, &root).await.unwrap();
            assert!(
                listing
                    .entries
                    .iter()
                    .all(|entry| entry.path.file_name().unwrap() != "linked-outside")
            );
            assert!(
                list_directory(&runner, &root, &root.join("linked-outside"))
                    .await
                    .is_err()
            );
            assert!(
                list_directory(&runner, &root, &root.join("linked-outside").join("child"),)
                    .await
                    .is_err()
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_listing_preserves_a_root_symlink_alias() {
        let temp = tempfile::tempdir().unwrap();
        let actual_root = temp.path().join("workspace");
        let root_alias = temp.path().join("workspace-alias");
        std::fs::create_dir(&actual_root).unwrap();
        std::fs::write(actual_root.join("visible"), "yes\n").unwrap();
        std::os::unix::fs::symlink(&actual_root, &root_alias).unwrap();
        let runner = ProcessRunner;

        let listing = list_directory(&runner, &root_alias, &root_alias)
            .await
            .unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].path, root_alias.join("visible"));
    }

    #[tokio::test]
    async fn workspace_python_ignores_startup_hooks_and_shadow_modules() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let marker = root.join("executed");
        let trap = "from pathlib import Path\nPath('executed').write_text('unsafe')\nraise RuntimeError('workspace Python executed')\n";
        for name in [
            "sitecustomize.py",
            "usercustomize.py",
            "json.py",
            "heapq.py",
        ] {
            std::fs::write(root.join(name), trap).unwrap();
        }
        std::fs::write(root.join("visible.txt"), "safe preview\n").unwrap();
        let command = python_command(LIST_DIRECTORY, root, &[]);
        assert_eq!(&command.argv[..4], &["python3", "-I", "-S", "-c"]);

        let listing = list_directory(&ProcessRunner, root, root).await.unwrap();
        assert_eq!(listing.entries.len(), 5);
        assert_eq!(
            read_file(&ProcessRunner, &root.join("visible.txt"), root)
                .await
                .unwrap(),
            "safe preview\n"
        );
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn directory_listing_is_directories_first_name_sorted_and_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("z-dir")).unwrap();
        std::fs::create_dir(root.join("a-dir")).unwrap();
        for index in 0..=DIRECTORY_LIST_LIMIT {
            std::fs::write(root.join(format!("file-{index:04}")), "x").unwrap();
        }
        let runner = ProcessRunner;

        let listing = list_directory(&runner, &root, &root).await.unwrap();
        assert!(listing.truncated);
        assert_eq!(listing.entries.len(), DIRECTORY_LIST_LIMIT);
        assert_eq!(listing.entries[0].path.file_name().unwrap(), "a-dir");
        assert!(listing.entries[0].is_dir);
        assert_eq!(listing.entries[1].path.file_name().unwrap(), "z-dir");
        assert!(listing.entries[1].is_dir);
        assert_eq!(listing.entries[2].path.file_name().unwrap(), "file-0000");
        assert!(!listing.entries[2].is_dir);
        assert_eq!(
            listing.entries.last().unwrap().path.file_name().unwrap(),
            "file-1997"
        );
    }
}
