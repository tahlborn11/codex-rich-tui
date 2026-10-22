//! Evidence of branches created by successful commands in one chat.
//!
//! A checkout's current HEAD is not ownership evidence. Recognize literal Git
//! creation commands, then let Outputs verify the exact named ref before saving it.
use std::path::Path;
use std::path::PathBuf;

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

/// Prune only confirmed missing refs. Probe failures preserve recorded ownership.
/// Durable tombstones keep old history or delayed writers from restoring it.
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
                let missing = matches!(
                    crate::outputs::data::association_branch_exists(runner.as_ref(), &association)
                        .await,
                    Ok(false)
                );
                missing.then_some(association)
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
                preferences.remove_association(association);
            }
            preferences.save_waiting(&path)?;
            Ok::<_, String>(
                removals
                    .into_iter()
                    .filter(|association| {
                        preferences
                            .association(&association.repo_root.to_string_lossy())
                            .is_none()
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
        // Simulate an older CLI rewriting its bookmark file without knowing
        // about branch ownership; the separate provenance file must survive.
        std::fs::write(&path, br#"{"repos":{},"selected_repo":null}"#).unwrap();
        let preferences = crate::outputs::Preferences::load_with_associations(&path);
        assert!(preferences.association("/project/api").is_some());
        assert!(preferences.association("/outside/worker").is_some());
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
