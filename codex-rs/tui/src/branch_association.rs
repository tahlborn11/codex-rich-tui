//! Branch usage observed in a chat's execution and file-change locations.
//!
//! Usage does not imply exclusive ownership. Existing literal creation evidence
//! is retained as a precise historical hint; new usage is independent of command syntax.
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::ThreadItem;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CreatedBranch {
    /// Checkout containing the new branch (the new worktree for `worktree add`).
    pub(crate) cwd: PathBuf,
    pub(crate) branch: String,
    pub(crate) evidence_id: String,
    pub(crate) completed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BranchUse {
    pub(crate) cwd: PathBuf,
    pub(crate) evidence_id: String,
    pub(crate) completed_at_ms: Option<i64>,
    pub(crate) inferred: bool,
}

/// Execution cwd and changed file locations come from structured tool items,
/// not shell text. A failed executed command can still be work on a branch.
pub(crate) fn uses_from_item(item: &ThreadItem) -> Vec<BranchUse> {
    let mut locations = Vec::new();
    let id = match item {
        ThreadItem::CommandExecution {
            id, cwd, status, ..
        } if *status != CommandExecutionStatus::Declined => {
            locations.push(PathBuf::from(cwd.as_str()));
            id
        }
        ThreadItem::FileChange {
            id,
            changes,
            status: codex_app_server_protocol::PatchApplyStatus::Completed,
            ..
        } => {
            locations.extend(
                changes
                    .iter()
                    .filter_map(|change| Path::new(&change.path).parent().map(Path::to_path_buf)),
            );
            id
        }
        _ => return Vec::new(),
    };
    locations.sort();
    locations.dedup();
    locations
        .into_iter()
        .take(128)
        .map(|cwd| BranchUse {
            cwd,
            evidence_id: format!("usage:{id}"),
            completed_at_ms: None,
            inferred: false,
        })
        .collect()
}

/// Historical cwd identifies a used checkout, not its old HEAD. Keep that
/// distinction in the persisted record and never overwrite precise evidence.
pub(crate) async fn verify_uses(
    runner: crate::workspace_command::WorkspaceCommandRunner,
    uses: Vec<BranchUse>,
) -> Vec<crate::outputs::BranchAssociation> {
    use futures::StreamExt;
    let mut latest = std::collections::BTreeMap::new();
    for usage in uses {
        latest.insert(usage.cwd.clone(), usage);
    }
    let mut pending = futures::stream::iter(latest.into_values().take(128))
        .map(|usage| {
            let runner = runner.clone();
            async move {
                crate::outputs::data::observe_branch(
                    runner.as_ref(),
                    &usage.cwd,
                    &usage.evidence_id,
                    usage.completed_at_ms,
                    usage.inferred,
                )
                .await
                .ok()
            }
        })
        .buffer_unordered(4);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut observed = Vec::new();
    while let Ok(Some(result)) = tokio::time::timeout_at(deadline, pending.next()).await {
        if let Some(association) = result {
            observed.push(association);
        }
    }
    observed.sort_by(|left, right| {
        left.completed_at_ms
            .cmp(&right.completed_at_ms)
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });
    observed
}

pub(crate) fn creations_from_item(item: &ThreadItem) -> Vec<CreatedBranch> {
    let ThreadItem::CommandExecution {
        id,
        command,
        cwd,
        status: CommandExecutionStatus::Completed,
        exit_code: Some(0),
        aggregated_output,
        ..
    } = item
    else {
        return Vec::new();
    };
    let cwd = Path::new(cwd.as_str());
    let found = creations_from_command(command, cwd, id);
    if !found.is_empty() {
        return found;
    }
    creations_with_status_confirmation(command, cwd, id, aggregated_output.as_deref())
}

/// A trailing status command masks the creation command's exit code. Accept
/// this narrow sequence only when Git also confirms that it created the branch.
fn creations_with_status_confirmation(
    command: &str,
    cwd: &Path,
    evidence_id: &str,
    output: Option<&str>,
) -> Vec<CreatedBranch> {
    if command.len() > 64 * 1024 {
        return Vec::new();
    }
    let argv = crate::exec_command::split_command_string(command);
    let script = codex_shell_command::bash::extract_bash_command(&argv)
        .map_or(command, |(_, script)| script);
    let Some(tree) = codex_shell_command::bash::try_parse_shell(script) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let nodes = root.named_children(&mut cursor).collect::<Vec<_>>();
    let [creation, status] = nodes.as_slice() else {
        return Vec::new();
    };
    if root.has_error() || creation.kind() != "command" || status.kind() != "command" {
        return Vec::new();
    }
    let separator = script[creation.end_byte()..status.start_byte()].trim();
    if separator != ";" && !separator.is_empty() {
        return Vec::new();
    }
    // Only a literal read-only status suffix may accompany the creation.
    let Some(status_commands) =
        codex_shell_command::bash::parse_shell_script_into_commands(&script[status.byte_range()])
    else {
        return Vec::new();
    };
    let [status_words] = status_commands.as_slice() else {
        return Vec::new();
    };
    if status_words.len() < 2
        || status_words[0] != "git"
        || status_words[1] != "status"
        || status_words[2..].iter().any(|word| {
            !matches!(
                word.as_str(),
                "--short" | "--branch" | "-s" | "-b" | "-sb" | "-bs"
            )
        })
    {
        return Vec::new();
    }
    let found = creations_from_command(&script[creation.byte_range()], cwd, evidence_id);
    let [branch] = found.as_slice() else {
        return Vec::new();
    };
    let confirmation = format!("Switched to a new branch '{}'", branch.branch);
    if output.and_then(|output| output.lines().next()) != Some(confirmation.as_str()) {
        return Vec::new();
    }
    found
}

/// Verify a bounded batch without allowing slow probes to block the viewer.
pub(crate) async fn verify_creations(
    runner: crate::workspace_command::WorkspaceCommandRunner,
    creations: Vec<CreatedBranch>,
) -> Vec<crate::outputs::BranchAssociation> {
    use futures::StreamExt;
    let skip = creations.len().saturating_sub(128);
    let mut pending = futures::stream::iter(creations.into_iter().skip(skip).enumerate())
        .map(|(index, creation)| {
            let runner = runner.clone();
            async move {
                (
                    index,
                    crate::outputs::data::verify_created_branch(runner.as_ref(), &creation).await,
                )
            }
        })
        .buffer_unordered(4);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut verified = Vec::new();
    while let Ok(Some((index, result))) = tokio::time::timeout_at(deadline, pending.next()).await {
        if let Ok(association) = result {
            verified.push((index, association));
        }
    }
    verified.sort_by_key(|(index, _)| *index);
    verified
        .into_iter()
        .map(|(_, association)| association)
        .collect()
}

/// Missing refs become historical entries. Probe failures leave them unchanged;
/// deletion never erases the chat's work history or creates a detach tombstone.
pub(crate) async fn prune_deleted_associations(
    runner: crate::workspace_command::WorkspaceCommandRunner,
    path: Option<PathBuf>,
    associations: Vec<crate::outputs::BranchAssociation>,
) -> Result<Vec<crate::outputs::BranchAssociation>, String> {
    use futures::StreamExt;
    let mut pending = futures::stream::iter(associations.into_iter().take(256))
        .map(|association| {
            let runner = runner.clone();
            async move {
                if association.archived {
                    return None;
                }
                let missing = matches!(
                    crate::outputs::data::association_branch_exists(runner.as_ref(), &association)
                        .await,
                    Ok(false)
                );
                missing.then(|| {
                    let mut historical = association;
                    historical.archived = true;
                    historical
                })
            }
        })
        .buffer_unordered(4);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut removed = Vec::new();
    while let Ok(Some(result)) = tokio::time::timeout_at(deadline, pending.next()).await {
        if let Some(association) = result {
            removed.push(association);
        }
    }
    if !removed.is_empty()
        && let Some(path) = path
    {
        let removals = removed.clone();
        removed = tokio::task::spawn_blocking(move || {
            let path = path.with_extension("branches.json");
            let mut preferences = crate::outputs::Preferences::load(&path);
            for association in &removals {
                preferences.record_association(association.clone());
            }
            preferences.save_waiting(&path)?;
            Ok::<_, String>(
                removals
                    .into_iter()
                    .filter(|association| {
                        preferences
                            .association_for(
                                &association.repo_root.to_string_lossy(),
                                &association.branch_ref,
                            )
                            .is_some_and(|stored| {
                                stored.archived && stored.evidence_id == association.evidence_id
                            })
                    })
                    .collect(),
            )
        })
        .await
        .map_err(|error| format!("Branch cleanup failed: {error}"))??;
    }
    Ok(removed)
}

pub(crate) async fn persist_associations(
    path: PathBuf,
    associations: Vec<crate::outputs::BranchAssociation>,
) -> Result<(), String> {
    let path = path.with_extension("branches.json");
    tokio::task::spawn_blocking(move || {
        let mut preferences = crate::outputs::Preferences::load(&path);
        for association in associations {
            preferences.record_association(association);
        }
        preferences.save_waiting(&path)
    })
    .await
    .map_err(|error| format!("Branch association persistence failed: {error}"))?
}

/// Store live observations only when they materially change the branch view.
/// Repeated tool events commonly see the same checked-out ref and commit; the
/// sidecar still receives new branches, new heads, archival upgrades, and a
/// precise observation replacing recovered history.
pub(crate) async fn persist_observations(
    path: PathBuf,
    associations: Vec<crate::outputs::BranchAssociation>,
) -> Result<bool, String> {
    let path = path.with_extension("branches.json");
    tokio::task::spawn_blocking(move || {
        let mut preferences = crate::outputs::Preferences::load(&path);
        let mut changed = false;
        for association in associations {
            let unchanged_live_observation = preferences
                .association(&association.repo_root.to_string_lossy())
                .is_some_and(|existing| {
                    existing.branch_ref == association.branch_ref
                        && !association.inferred
                        && !existing.inferred
                        && !existing.archived
                        && existing.last_head == association.last_head
                        && existing.last_base == association.last_base
                });
            if !unchanged_live_observation {
                changed |= preferences.record_association(association);
            }
        }
        if changed {
            preferences.save_waiting(&path)?;
        }
        Ok(changed)
    })
    .await
    .map_err(|error| format!("Live branch observation persistence failed: {error}"))?
}

/// Claim the bounded, one-time historical cwd recovery pass before any Git
/// probe starts. A completed claim is durable even if probing later finds no
/// repository or is interrupted.
pub(crate) async fn claim_history_recovery(path: PathBuf) -> Result<bool, String> {
    let path = path.with_extension("branches.json");
    tokio::task::spawn_blocking(move || {
        crate::outputs::Preferences::default().claim_history_usage_backfilled(&path)
    })
    .await
    .map_err(|error| format!("Historical branch recovery claim failed: {error}"))?
}

pub(crate) async fn detach_association(
    path: PathBuf,
    association: crate::outputs::BranchAssociation,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = path.with_extension("branches.json");
        let mut preferences = crate::outputs::Preferences::load(&path);
        if let Some(current) = preferences
            .association_for(
                &association.repo_root.to_string_lossy(),
                &association.branch_ref,
            )
            .cloned()
        {
            let detached_at_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis().try_into().unwrap_or(i64::MAX))
                .unwrap_or_default();
            static DETACH_SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let sequence = DETACH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let tombstone = crate::outputs::BranchAssociation {
                evidence_id: format!("detach:{detached_at_ms}:{sequence}"),
                completed_at_ms: Some(detached_at_ms),
                ..current.clone()
            };
            preferences.detach_association(&current, tombstone);
        }
        preferences.save_waiting(&path)
    })
    .await
    .map_err(|error| format!("Branch detachment failed: {error}"))?
}

fn creations_from_command(command: &str, cwd: &Path, evidence_id: &str) -> Vec<CreatedBranch> {
    if !cwd.is_absolute() || command.len() > 64 * 1024 {
        return Vec::new();
    }
    let argv = crate::exec_command::split_command_string(command);
    let script = codex_shell_command::bash::extract_bash_command(&argv)
        .map_or(command, |(_, script)| script);
    let Some(commands) = codex_shell_command::bash::parse_shell_script_into_commands(script) else {
        return Vec::new();
    };
    let Some(tree) = codex_shell_command::bash::try_parse_shell(script) else {
        return Vec::new();
    };
    // Overall exit zero proves every command succeeded only for an && chain.
    // Reject pipelines, OR recovery, semicolons and independent newline commands.
    let mut nodes = vec![tree.root_node()];
    let mut ands = 0;
    while let Some(node) = nodes.pop() {
        match node.kind() {
            "&&" => ands += 1,
            "||" | ";" | "|" | "!" | "&" => return Vec::new(),
            _ => {}
        }
        let mut cursor = node.walk();
        nodes.extend(node.children(&mut cursor));
    }
    if commands.is_empty() || ands + 1 != commands.len() {
        return Vec::new();
    }
    let mut cwd = cwd.to_path_buf();
    let mut found = Vec::new();
    for args in commands {
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        if let ["cd", path] | ["cd", "--", path] = args.as_slice() {
            // Bare relative names can be redirected through CDPATH. Only
            // paths whose meaning is independent of that shell setting count.
            if !Path::new(path).is_absolute()
                && !matches!(*path, "." | "..")
                && !path.starts_with("./")
                && !path.starts_with("../")
            {
                return Vec::new();
            }
            let joined = cwd.join(path);
            cwd = PathBuf::new();
            for component in joined.components() {
                match component {
                    std::path::Component::ParentDir => {
                        cwd.pop();
                    }
                    std::path::Component::CurDir => {}
                    _ => cwd.push(component.as_os_str()),
                }
            }
            continue;
        }
        if args.first() != Some(&"git") {
            // Later commands cannot undo evidence of an earlier successful
            // creation, but their working-directory effects are unknown.
            break;
        }
        let mut git_cwd = cwd.clone();
        let mut rest = &args[1..];
        while let ["-C", path, tail @ ..] = rest {
            if path.is_empty() {
                return Vec::new();
            }
            git_cwd = git_cwd.join(path);
            rest = tail;
        }
        let creation = match rest {
            [kind @ ("switch" | "checkout"), args @ ..] => {
                parse_creation_args(kind, args).map(|(branch, _)| (git_cwd, branch))
            }
            ["worktree", "add", args @ ..] => parse_creation_args("worktree", args)
                .and_then(|(branch, path)| path.map(|path| (git_cwd.join(path), branch))),
            _ => None,
        };
        if let Some((cwd, branch)) = creation
            && !branch.is_empty()
            && !branch.starts_with('-')
        {
            let evidence_id = if found.is_empty() {
                evidence_id.to_owned()
            } else {
                format!("{evidence_id}#{:04}", found.len())
            };
            found.push(CreatedBranch {
                cwd,
                branch: branch.to_owned(),
                evidence_id,
                completed_at_ms: None,
            });
        }
    }
    found
}

/// Accept benign creation flags in either order; reset/reuse flags stay excluded.
fn parse_creation_args<'a>(kind: &str, args: &[&'a str]) -> Option<(&'a str, Option<&'a str>)> {
    let mut branch = None;
    let mut operands = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index];
        index += 1;
        match arg {
            "-q" | "--quiet" | "-t" | "--track" | "--track=direct" | "--track=inherit"
            | "--no-track" => {}
            "-c" | "--create" | "-qc" if kind == "switch" => {
                if branch.is_some() {
                    return None;
                }
                branch = Some(*args.get(index)?);
                index += 1;
            }
            "-b" | "-qb" if kind != "switch" => {
                if branch.is_some() {
                    return None;
                }
                branch = Some(*args.get(index)?);
                index += 1;
            }
            "--" => {
                operands.extend_from_slice(&args[index..]);
                break;
            }
            _ if arg.starts_with("--create=") && kind == "switch" => {
                if branch.is_some() {
                    return None;
                }
                branch = arg.strip_prefix("--create=");
            }
            _ if arg.starts_with('-') => return None,
            _ => operands.push(arg),
        }
    }
    let branch = branch.filter(|branch| !branch.is_empty() && !branch.starts_with('-'))?;
    if kind == "worktree" {
        if operands.is_empty() || operands.len() > 2 || operands[0].starts_with('-') {
            return None;
        }
        Some((branch, Some(operands[0])))
    } else if operands.len() <= 1 {
        Some((branch, None))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_comes_from_structured_context_not_shell_syntax_or_exit_success() {
        for command in [
            "python3 script.py",
            "git fetch origin\ngit worktree add -b next ../next",
            "./opaque-wrapper",
        ] {
            let item: ThreadItem = serde_json::from_value(serde_json::json!({
                "type":"commandExecution", "id":"work", "command":command,
                "cwd":"/project/checkout", "status":"completed", "exitCode":1,
                "commandActions":[],
            }))
            .unwrap();
            let uses = uses_from_item(&item);
            assert_eq!(uses.len(), 1);
            assert_eq!(uses[0].cwd, Path::new("/project/checkout"));
            assert_eq!(uses[0].evidence_id, "usage:work");
        }
        let declined: ThreadItem = serde_json::from_value(serde_json::json!({
            "type":"commandExecution", "id":"declined", "command":"git status",
            "cwd":"/project/checkout", "status":"declined", "exitCode":null,
            "commandActions":[],
        }))
        .unwrap();
        assert!(uses_from_item(&declined).is_empty());
    }

    #[test]
    fn captures_literal_creation_names_and_checkout_paths() {
        for (command, root, branch) in [
            ("git switch -c feature/chat", "/project", "feature/chat"),
            ("git switch -q -c topic", "/project", "topic"),
            ("git switch -t -c topic origin/topic", "/project", "topic"),
            ("git checkout -qb topic", "/project", "topic"),
            (
                "git checkout --track -b topic origin/topic",
                "/project",
                "topic",
            ),
            (
                "git worktree add --track -b topic /outside origin/topic",
                "/outside",
                "topic",
            ),
            (
                "cd './repo one' && git checkout -b topic",
                "/project/repo one",
                "topic",
            ),
            (
                "git -C repo worktree add -b topic ../worktree",
                "/project/repo/../worktree",
                "topic",
            ),
            (
                "git -C repo switch --create topic main",
                "/project/repo",
                "topic",
            ),
            ("/bin/zsh -lc 'git switch -c topic'", "/project", "topic"),
        ] {
            let found = creations_from_command(command, Path::new("/project"), "call");
            assert_eq!(found.len(), 1, "{command}");
            assert_eq!(found[0].cwd, Path::new(root), "{command}");
            assert_eq!(found[0].branch, branch);
            assert_eq!(found[0].evidence_id, "call");
        }
    }

    #[test]
    fn rejects_ambiguous_failed_or_existing_branch_commands() {
        for command in [
            "git switch topic",
            "git checkout -B topic",
            "git switch -C topic",
            "git branch topic",
            "echo 'git switch -c topic'",
            "git switch -c topic || true",
            "git switch -c topic; true",
            "git switch -c topic\ntrue",
            "git switch -c topic | cat",
            "! git switch -c existing",
            "git switch -c existing &",
            "git switch -c $(echo topic)",
            "pushd repo && git switch -c topic",
            "cd repo && git switch -c topic",
            "git worktree add --detach repo",
        ] {
            assert!(
                creations_from_command(command, Path::new("/project"), "call").is_empty(),
                "{command}"
            );
        }
    }

    #[test]
    fn recognizes_creation_followed_by_status_only_with_git_confirmation() {
        for separator in ["; ", "\n"] {
            let command = format!(
                "/bin/zsh -lc 'git switch -c codex/throwaway-2026-10-22{separator}git status --short --branch'"
            );
            for (output, expected) in [
                (
                    Some(
                        "Switched to a new branch 'codex/throwaway-2026-10-22'\n## codex/throwaway-2026-10-22\n",
                    ),
                    1,
                ),
                (
                    Some(
                        "fatal: a branch named 'codex/throwaway-2026-10-22' already exists\n## main\n",
                    ),
                    0,
                ),
                (Some("Switched to a new branch 'other'\n"), 0),
                (None, 0),
            ] {
                let item: ThreadItem = serde_json::from_value(serde_json::json!({
                    "type":"commandExecution", "id":"creation", "command":command,
                    "cwd":"/project/capim-infra", "status":"completed", "exitCode":0,
                    "commandActions":[], "aggregatedOutput":output,
                }))
                .unwrap();
                let found = creations_from_item(&item);
                assert_eq!(found.len(), expected, "{command} {output:?}");
                if let Some(branch) = found.first() {
                    assert_eq!(branch.branch, "codex/throwaway-2026-10-22");
                    assert_eq!(branch.cwd, Path::new("/project/capim-infra"));
                }
            }
        }
    }

    #[test]
    fn confirmation_does_not_allow_opaque_or_mutating_sequences() {
        for script in [
            "git switch -c topic || git status",
            "git switch -c topic & git status",
            "git switch -c topic; echo fake; git status",
            "git switch -c topic; git switch main",
            "git switch -c topic; git status $(echo --short)",
            "git switch -c topic; git status > file",
            "git switch -c topic; git status; echo fake",
        ] {
            assert!(
                creations_with_status_confirmation(
                    script,
                    Path::new("/project"),
                    "call",
                    Some("Switched to a new branch 'topic'\n"),
                )
                .is_empty(),
                "{script}"
            );
        }
    }

    #[test]
    fn only_completed_zero_exit_commands_are_creation_evidence() {
        for (status, exit_code, expected) in [
            ("completed", Some(0), 1),
            ("completed", Some(1), 0),
            ("failed", Some(0), 0),
            ("completed", None, 0),
            ("inProgress", Some(0), 0),
        ] {
            let item: ThreadItem = serde_json::from_value(serde_json::json!({
                "type":"commandExecution", "id":"creation", "command":"git switch -c topic",
                "cwd":"/project", "status":status, "exitCode":exit_code, "commandActions":[],
            }))
            .unwrap();
            assert_eq!(
                creations_from_item(&item).len(),
                expected,
                "{status} {exit_code:?}"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_history_and_live_persistence_wait_and_merge() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thread.json");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("branches.lock"))
            .unwrap();
        lock.lock().unwrap();
        let make = |root: &str, id: &str, completed_at_ms| crate::outputs::BranchAssociation {
            repo_root: root.into(),
            branch_ref: "refs/heads/topic".into(),
            evidence_id: id.into(),
            completed_at_ms: Some(completed_at_ms),
            ..Default::default()
        };
        let history = tokio::spawn(persist_associations(
            path.clone(),
            vec![make("/project/api", "old", 10)],
        ));
        let live = tokio::spawn(persist_associations(
            path.clone(),
            vec![make("/outside/worker", "live", 20)],
        ));
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(!history.is_finished());
        assert!(!live.is_finished());
        drop(lock);
        history.await.unwrap().unwrap();
        live.await.unwrap().unwrap();
        // Simulate an older CLI rewriting its bookmark file without branch-use
        // provenance; the separate sidecar must survive.
        std::fs::write(&path, br#"{"repos":{},"selected_repo":null}"#).unwrap();
        let preferences = crate::outputs::Preferences::load_with_associations(&path);
        assert!(preferences.association("/project/api").is_some());
        assert!(preferences.association("/outside/worker").is_some());
    }

    #[tokio::test]
    async fn unchanged_live_observation_does_not_rewrite_or_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thread.json");
        let observation = crate::outputs::BranchAssociation {
            repo_root: "/project".into(),
            branch_ref: "refs/heads/topic".into(),
            evidence_id: "usage:first".into(),
            completed_at_ms: Some(10),
            last_head: Some("head".into()),
            last_base: Some("base".into()),
            ..Default::default()
        };
        assert!(
            persist_observations(path.clone(), vec![observation.clone()])
                .await
                .unwrap()
        );
        let repeat = crate::outputs::BranchAssociation {
            evidence_id: "usage:repeat".into(),
            completed_at_ms: Some(20),
            ..observation.clone()
        };
        assert!(
            !persist_observations(path.clone(), vec![repeat])
                .await
                .unwrap()
        );
        let advanced = crate::outputs::BranchAssociation {
            evidence_id: "usage:advanced".into(),
            completed_at_ms: Some(30),
            last_head: Some("next-head".into()),
            ..observation
        };
        assert!(persist_observations(path, vec![advanced]).await.unwrap());
    }

    #[tokio::test]
    async fn returning_to_unchanged_branch_updates_last_used_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thread.json");
        let used = |branch: &str, time| crate::outputs::BranchAssociation {
            repo_root: "/repo".into(),
            branch_ref: format!("refs/heads/{branch}"),
            evidence_id: format!("usage:{time}"),
            completed_at_ms: Some(time),
            last_head: Some("unchanged-head".into()),
            last_base: Some("base".into()),
            ..Default::default()
        };
        for observation in [used("a", 10), used("b", 20), used("a", 30)] {
            assert!(
                persist_observations(path.clone(), vec![observation])
                    .await
                    .unwrap()
            );
        }
        assert_eq!(
            crate::outputs::Preferences::load_with_associations(&path)
                .association("/repo")
                .unwrap()
                .branch_ref,
            "refs/heads/a"
        );
    }

    #[test]
    fn records_multiple_repositories_in_one_successful_chain() {
        let found = creations_from_command(
            "git -C api switch -c topic && git -C ui checkout -b topic",
            Path::new("/project"),
            "call",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].cwd, Path::new("/project/api"));
        assert_eq!(found[1].cwd, Path::new("/project/ui"));
        assert_eq!(found[1].evidence_id, "call#0001");
    }
}
