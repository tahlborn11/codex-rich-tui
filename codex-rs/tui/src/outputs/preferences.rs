use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::fs::{self};
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

const MAX_PREFERENCES_BYTES: u64 = 1024 * 1024;

#[derive(Default, Deserialize, Serialize)]
pub(crate) struct Preferences {
    #[serde(default)]
    repos: BTreeMap<String, RepoBookmark>,
    #[serde(default)]
    selected_repo: Option<String>,
    /// Verified provenance is intentionally distinct from the user's manually
    /// selected comparison bookmarks.  Existing bookmarks must never imply
    /// that a branch belongs to this chat.
    #[serde(default)]
    associations: BTreeMap<String, BranchAssociation>,
    /// A removed association remains as provenance so a stale history replay
    /// cannot make a deleted branch appear in this chat again.
    #[serde(default)]
    removed_associations: BTreeMap<String, BranchAssociation>,
    #[serde(skip)]
    dirty: BTreeSet<String>,
    #[serde(skip)]
    selected_dirty: bool,
    #[serde(skip)]
    association_dirty: BTreeSet<String>,
    #[serde(skip)]
    removed_association_dirty: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct BranchAssociation {
    pub(crate) repo_root: PathBuf,
    pub(crate) branch_ref: String,
    pub(crate) evidence_id: String,
    #[serde(default)]
    pub(crate) completed_at_ms: Option<i64>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct RepoBookmark {
    pub(crate) target: Option<String>,
    pub(crate) base: Option<String>,
    #[serde(default)]
    pub(crate) target_is_explicit: bool,
}

impl Preferences {
    /// Provenance uses a separate file so older clients saving viewer bookmarks
    /// cannot discard fields they do not understand.
    pub(crate) fn load_with_associations(path: &Path) -> Self {
        let mut preferences = Self::load(path);
        let provenance = Self::load(&path.with_extension("branches.json"));
        preferences.associations = provenance.associations;
        preferences.removed_associations = provenance.removed_associations;
        preferences
    }
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
    pub(crate) fn association(&self, identity: &str) -> Option<&BranchAssociation> {
        self.associations.get(identity)
    }
    pub(crate) fn associations(&self) -> impl Iterator<Item = &BranchAssociation> {
        self.associations.values()
    }
    /// Records verified ownership evidence. Returns false when the identical
    /// association is already present, avoiding an unnecessary persistence
    /// write during repeated refreshes.
    pub(crate) fn record_association(&mut self, association: BranchAssociation) -> bool {
        let identity = association.repo_root.to_string_lossy().into_owned();
        // Timestamp-less history has no ordering proof, so it may not undo a
        // deletion. A later, timestamped branch creation may establish a new
        // association for this checkout.
        if self
            .removed_associations
            .get(&identity)
            .is_some_and(|removed| !association_is_newer(removed, &association))
        {
            return false;
        }
        let Some(existing) = self.associations.get(&identity) else {
            self.association_dirty.insert(identity.clone());
            self.associations.insert(identity, association);
            return true;
        };
        if existing == &association {
            return false;
        }
        // During one chronological history replay, timestamp-less candidates
        // still need to advance in memory. The merge under the OS lock below
        // prevents such unknown-age evidence from replacing disk provenance.
        let may_replace = self.association_dirty.contains(&identity)
            && existing.completed_at_ms.is_none()
            && association.completed_at_ms.is_none()
            || association_is_newer(existing, &association);
        if !may_replace {
            return false;
        }
        self.association_dirty.insert(identity.clone());
        // Callers submit history in chronological order.  Replacing this
        // identity means the newest verified evidence wins without promoting
        // any manual bookmark to chat ownership.
        self.associations.insert(identity, association);
        true
    }
    /// Removes only the association whose verified evidence was observed to
    /// disappear. The tombstone is merged conditionally under the file lock,
    /// so this cannot delete a newer branch created by another view.
    pub(crate) fn remove_association(&mut self, association: &BranchAssociation) -> bool {
        let identity = association.repo_root.to_string_lossy().into_owned();
        let removed_active = self.associations.get(&identity) == Some(association);
        if removed_active {
            self.associations.remove(&identity);
            self.association_dirty.remove(&identity);
        }
        let update_tombstone = self
            .removed_associations
            .get(&identity)
            .is_none_or(|existing| association_is_newer(existing, association));
        if update_tombstone {
            self.removed_associations
                .insert(identity.clone(), association.clone());
        }
        // Always persist the deletion even if this view already had the same
        // tombstone: it may have been loaded before another stale writer.
        self.removed_association_dirty.insert(identity);
        removed_active
    }
    /// Refreshes only verified provenance. Unsaved bookmark choices remain
    /// local to this view, and no refreshed values are marked dirty.
    pub(crate) fn refresh_associations(&mut self, path: &Path) {
        let provenance = Self::load(&path.with_extension("branches.json"));
        self.associations = provenance.associations;
        self.removed_associations = provenance.removed_associations;
        self.association_dirty.clear();
        self.removed_association_dirty.clear();
    }
    pub(crate) fn selected_repo(&self) -> Option<&str> {
        self.selected_repo.as_deref()
    }
    pub(crate) fn set_selected_repo(&mut self, identity: Option<String>) {
        self.selected_repo = identity;
        self.selected_dirty = true;
    }
    pub(crate) fn save(&mut self, path: &Path) -> Result<(), String> {
        self.save_with_lock(path, false)
    }
    /// Bookmark writes must not replace provenance loaded from its sidecar.
    pub(crate) fn save_bookmarks(&mut self, path: &Path) -> Result<(), String> {
        let associations = std::mem::take(&mut self.associations);
        let removed_associations = std::mem::take(&mut self.removed_associations);
        let association_dirty = std::mem::take(&mut self.association_dirty);
        let removed_association_dirty = std::mem::take(&mut self.removed_association_dirty);
        let result = self.save(path);
        self.associations = associations;
        self.removed_associations = removed_associations;
        self.association_dirty = association_dirty;
        self.removed_association_dirty = removed_association_dirty;
        result
    }
    /// Persists while waiting for the cross-process lock. Call only from a
    /// blocking background task so an observer is not dropped when another
    /// view is saving preferences.
    pub(crate) fn save_waiting(&mut self, path: &Path) -> Result<(), String> {
        self.save_with_lock(path, true)
    }
    fn save_with_lock(&mut self, path: &Path, wait_for_lock: bool) -> Result<(), String> {
        let parent = path.parent().ok_or("preference path has no parent")?;
        fs::create_dir_all(parent).map_err(|e| format!("create preferences directory: {e}"))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .map_err(|e| format!("open preferences lock: {e}"))?;
        if wait_for_lock {
            lock.lock()
                .map_err(|error| format!("lock preferences: {error}"))?;
        } else {
            lock.try_lock()
                .map_err(|_| "preferences are busy; choices remain in this view".to_string())?;
        }
        let mut merged = Self::load(path);
        for key in &self.dirty {
            if let Some(value) = self.repos.get(key) {
                merged.repos.insert(key.clone(), value.clone());
            }
        }
        if self.selected_dirty {
            merged.selected_repo = self.selected_repo.clone();
        }
        // First merge and apply removals. A removal deletes only the exact
        // observed evidence; a concurrent newer creation is kept.
        for key in &self.removed_association_dirty {
            let Some(removed) = self.removed_associations.get(key) else {
                continue;
            };
            let merged_tombstone = merged
                .removed_associations
                .entry(key.clone())
                .or_insert_with(|| removed.clone());
            if association_is_newer(merged_tombstone, removed) {
                *merged_tombstone = removed.clone();
            }
            if merged.associations.get(key) == Some(removed) {
                merged.associations.remove(key);
            }
        }
        for key in &self.association_dirty {
            if let Some(value) = self.associations.get(key) {
                if merged
                    .removed_associations
                    .get(key)
                    .is_some_and(|removed| !association_is_newer(removed, value))
                {
                    continue;
                }
                let may_replace = merged
                    .associations
                    .get(key)
                    .is_none_or(|existing| association_is_newer(existing, value));
                if may_replace {
                    merged.associations.insert(key.clone(), value.clone());
                }
            }
        }
        if merged.repos.len() > 256 {
            return Err("Too many repository bookmarks for this chat.".into());
        }
        if merged.associations.len() > 256 {
            return Err("Too many verified branch associations for this chat.".into());
        }
        if merged.removed_associations.len() > 256 {
            return Err("Too many removed branch associations for this chat.".into());
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
        self.associations = merged.associations;
        self.removed_associations = merged.removed_associations;
        self.dirty.clear();
        self.selected_dirty = false;
        self.association_dirty.clear();
        self.removed_association_dirty.clear();
        Ok(())
    }
}

/// Returns true only when the candidate is known to be newer. Timestamp-less
/// evidence can seed a missing association, but can never displace evidence
/// already persisted by another view. Equal timestamps use evidence ID as a
/// deterministic tie-breaker; it establishes stable ordering, not chronology
/// finer than milliseconds.
fn association_is_newer(existing: &BranchAssociation, candidate: &BranchAssociation) -> bool {
    match (existing.completed_at_ms, candidate.completed_at_ms) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(existing_ms), Some(candidate_ms)) => {
            candidate_ms > existing_ms
                || (candidate_ms == existing_ms && candidate.evidence_id > existing.evidence_id)
        }
    }
}

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;
