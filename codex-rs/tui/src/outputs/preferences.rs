use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::fs::{self};
use std::io::Read;
use std::io::Write;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;

const MAX_PREFERENCES_BYTES: u64 = 1024 * 1024;

#[derive(Default, Deserialize, Serialize)]
pub(crate) struct Preferences {
    #[serde(default)]
    repos: BTreeMap<String, RepoBookmark>,
    #[serde(default)]
    selected_repo: Option<String>,
    #[serde(skip)]
    dirty: BTreeSet<String>,
    #[serde(skip)]
    selected_dirty: bool,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct RepoBookmark {
    pub(crate) target: Option<String>,
    pub(crate) base: Option<String>,
}

impl Preferences {
    pub(crate) fn load(path: &Path) -> Self {
        let Ok(file) = std::fs::File::open(path) else {
            return Self::default();
        };
        if !file
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_PREFERENCES_BYTES)
        {
            return Self::default();
        }
        let mut bytes = Vec::new();
        if file
            .take(MAX_PREFERENCES_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_PREFERENCES_BYTES
        {
            return Self::default();
        }
        serde_json::from_slice(&bytes).unwrap_or_default()
    }
    pub(crate) fn bookmark(&self, key: &str) -> Option<&RepoBookmark> {
        self.repos.get(key)
    }
    pub(crate) fn set(&mut self, key: String, value: RepoBookmark) {
        self.dirty.insert(key.clone());
        self.repos.insert(key, value);
    }
    pub(crate) fn selected_repo(&self) -> Option<&str> {
        self.selected_repo.as_deref()
    }
    pub(crate) fn set_selected_repo(&mut self, identity: Option<String>) {
        self.selected_repo = identity;
        self.selected_dirty = true;
    }
    pub(crate) fn save(&mut self, path: &Path) -> Result<(), String> {
        let parent = path.parent().ok_or("preference path has no parent")?;
        fs::create_dir_all(parent).map_err(|e| format!("create preferences directory: {e}"))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .map_err(|e| format!("open preferences lock: {e}"))?;
        lock.try_lock()
            .map_err(|_| "preferences are busy; choices remain in this view".to_string())?;
        let mut merged = Self::load(path);
        for key in &self.dirty {
            if let Some(value) = self.repos.get(key) {
                merged.repos.insert(key.clone(), value.clone());
            }
        }
        if self.selected_dirty {
            merged.selected_repo = self.selected_repo.clone();
        }
        if merged.repos.len() > 256 {
            return Err("Too many repository bookmarks for this chat.".into());
        }
        let bytes = serde_json::to_vec(&merged).map_err(|e| format!("encode preferences: {e}"))?;
        if bytes.len() as u64 > MAX_PREFERENCES_BYTES {
            return Err("Repository bookmarks exceed their size limit.".into());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| format!("create preferences file: {e}"))?;
        temporary
            .write_all(&bytes)
            .and_then(|_| temporary.flush())
            .map_err(|e| format!("write preferences: {e}"))?;
        temporary
            .persist(path)
            .map_err(|e| format!("replace preferences: {}", e.error))?;
        self.repos = merged.repos;
        self.selected_repo = merged.selected_repo;
        self.dirty.clear();
        self.selected_dirty = false;
        Ok(())
    }
}

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;
