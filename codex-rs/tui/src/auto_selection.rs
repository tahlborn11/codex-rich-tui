//! TUI-only persistence for each chat's `Auto` versus manual model selection.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use codex_protocol::ThreadId;

const AUTO_SELECTION_DIRECTORY: &str = "auto-selections";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoSelection {
    Auto,
    Manual,
}

impl AutoSelection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

pub(crate) async fn load(
    codex_home: &Path,
    thread_id: ThreadId,
) -> anyhow::Result<Option<AutoSelection>> {
    let path = selection_path(codex_home, thread_id);
    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    match contents.trim() {
        "auto" => Ok(Some(AutoSelection::Auto)),
        "manual" => Ok(Some(AutoSelection::Manual)),
        value => anyhow::bail!("invalid Auto selection `{value}` in {}", path.display()),
    }
}

pub(crate) async fn persist(
    codex_home: &Path,
    thread_id: ThreadId,
    selection: AutoSelection,
) -> anyhow::Result<()> {
    let path = selection_path(codex_home, thread_id);
    tokio::task::spawn_blocking(move || {
        codex_utils_path::write_atomically(&path, selection.as_str())
    })
    .await
    .context("join Auto selection persistence task")??;
    Ok(())
}

fn selection_path(codex_home: &Path, thread_id: ThreadId) -> PathBuf {
    codex_home
        .join("tui")
        .join(AUTO_SELECTION_DIRECTORY)
        .join(thread_id.to_string())
}

#[cfg(test)]
#[path = "auto_selection_tests.rs"]
mod tests;
