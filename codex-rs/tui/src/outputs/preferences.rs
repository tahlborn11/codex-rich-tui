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
    /// A detach tombstone remains as provenance so a stale history replay
    /// cannot make a detached branch appear in this chat again.
    #[serde(default)]
    removed_associations: BTreeMap<String, BranchAssociation>,
    /// History usage is recovered once, after the complete saved transcript is
    /// available.  This lives with provenance rather than viewer bookmarks.
    #[serde(default)]
    history_usage_backfilled: bool,
    #[serde(skip)]
    dirty: BTreeSet<String>,
    #[serde(skip)]
    selected_dirty: bool,
    #[serde(skip)]
    association_dirty: BTreeSet<String>,
    #[serde(skip)]
    removed_association_dirty: BTreeSet<String>,
    #[serde(skip)]
    history_usage_backfilled_dirty: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct BranchAssociation {
    pub(crate) repo_root: PathBuf,
    pub(crate) branch_ref: String,
    pub(crate) evidence_id: String,
    #[serde(default)]
    pub(crate) completed_at_ms: Option<i64>,
    /// Commit identities make historical branches useful after ref deletion.
    #[serde(default)]
    pub(crate) last_head: Option<String>,
    #[serde(default)]
    pub(crate) last_base: Option<String>,
    /// A missing ref is history, not a detach request.
    #[serde(default)]
    pub(crate) archived: bool,
    /// True when a checkout observation, rather than explicit provenance,
    /// associated this branch with the chat.
    #[serde(default)]
    pub(crate) inferred: bool,
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
        preferences.history_usage_backfilled = provenance.history_usage_backfilled;
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
        let mut preferences: Self = serde_json::from_slice(&bytes).unwrap_or_default();
        preferences.migrate_association_keys();
        preferences
    }
    pub(crate) fn bookmark(&self, key: &str) -> Option<&RepoBookmark> {
        self.repos.get(key)
    }
    pub(crate) fn set(&mut self, key: String, value: RepoBookmark) {
        self.dirty.insert(key.clone());
        self.repos.insert(key, value);
    }
    pub(crate) fn association(&self, identity: &str) -> Option<&BranchAssociation> {
        self.associations_for(identity).into_iter().next()
    }
    pub(crate) fn associations(&self) -> impl Iterator<Item = &BranchAssociation> {
        self.associations.values()
    }
    pub(crate) fn associations_for(&self, identity: &str) -> Vec<&BranchAssociation> {
        let mut associations = self
            .associations
            .values()
            .filter(|association| association.repo_root == Path::new(identity))
            .collect::<Vec<_>>();
        associations.sort_by(|left, right| association_order(right, left));
        associations
    }
    pub(crate) fn association_for(
        &self,
        identity: &str,
        branch_ref: &str,
    ) -> Option<&BranchAssociation> {
        self.associations
            .get(&association_key(identity, branch_ref))
    }
    pub(crate) fn history_usage_backfilled(&self) -> bool {
        self.history_usage_backfilled
    }
    /// Mark only after the full persisted transcript has been examined. The
    /// marker is monotonic so a stale bookmark writer cannot request a replay.
    pub(crate) fn mark_history_usage_backfilled(&mut self) {
        if !self.history_usage_backfilled {
            self.history_usage_backfilled = true;
            self.history_usage_backfilled_dirty = true;
        }
    }
    /// Atomically claim the single historical usage recovery pass. The caller
    /// must use this before probing checkout paths, so a second viewer cannot
    /// reinterpret old history after the branch has changed.
    pub(crate) fn claim_history_usage_backfilled(&mut self, path: &Path) -> Result<bool, String> {
        self.mark_history_usage_backfilled();
        self.save_with_lock(path, true)
    }
    /// Records verified branch-use evidence. Returns false when the identical
    /// association is already present, avoiding an unnecessary persistence
    /// write during repeated refreshes.
    pub(crate) fn record_association(&mut self, association: BranchAssociation) -> bool {
        let key = association_key_for(&association);
        // A tombstone affects only this exact checkout/ref pair. A strictly
        // newer observation can reattach it; equal evidence may only improve
        // historical metadata, never resurrect an explicitly detached branch.
        if self
            .removed_associations
            .get(&key)
            .is_some_and(|removed| !association_is_newer(removed, &association))
        {
            return false;
        }
        let Some(existing) = self.associations.get(&key) else {
            self.association_dirty.insert(key.clone());
            self.associations.insert(key, association);
            return true;
        };
        if existing == &association {
            return false;
        }
        // Recovered checkout observations are useful to seed a missing branch,
        // but must never replace direct evidence for that same branch.
        if association.inferred && !existing.inferred {
            return false;
        }
        if !association.inferred && existing.inferred {
            // A legacy creation record can be more precise than recovered
            // usage, but it cannot turn archived inferred history back into
            // a live branch unless it is chronologically newer. Equal
            // evidence may still upgrade the provenance classification while
            // preserving the archival state.
            if existing.archived && !association_is_newer(existing, &association) {
                if !same_evidence(existing, &association) {
                    return false;
                }
                let mut replacement = merge_association(existing, &association);
                replacement.archived = true;
                self.association_dirty.insert(key.clone());
                self.associations.insert(key, replacement);
                return true;
            }
            let replacement = merge_association(existing, &association);
            self.association_dirty.insert(key.clone());
            self.associations.insert(key, replacement);
            return true;
        }
        if same_evidence(existing, &association) {
            // An archived observation is allowed to enrich an equal event
            // with stable commit IDs. The reverse direction cannot make old
            // history active again.
            if existing.archived && !association.archived {
                return false;
            }
            let replacement = merge_association(existing, &association);
            self.association_dirty.insert(key.clone());
            self.associations.insert(key, replacement);
            return true;
        }
        // During one chronological history replay, timestamp-less candidates
        // still need to advance in memory. The merge under the OS lock below
        // prevents such unknown-age evidence from replacing disk provenance.
        let may_replace = self.association_dirty.contains(&key)
            && existing.completed_at_ms.is_none()
            && association.completed_at_ms.is_none()
            || association_is_newer(existing, &association);
        if !may_replace {
            return false;
        }
        self.association_dirty.insert(key.clone());
        self.associations.insert(key, association);
        true
    }
    /// Removes only the association whose verified evidence was observed to
    /// disappear. The tombstone is merged conditionally under the file lock,
    /// so this cannot delete a newer branch created by another view.
    pub(crate) fn remove_association(&mut self, association: &BranchAssociation) -> bool {
        let key = association_key_for(association);
        let removed_active = self.associations.get(&key) == Some(association);
        if removed_active {
            self.associations.remove(&key);
            self.association_dirty.remove(&key);
        }
        let update_tombstone = self
            .removed_associations
            .get(&key)
            .is_none_or(|existing| association_is_newer(existing, association));
        if update_tombstone {
            self.removed_associations
                .insert(key.clone(), association.clone());
        }
        // Always persist the deletion even if this view already had the same
        // tombstone: it may have been loaded before another stale writer.
        self.removed_association_dirty.insert(key);
        removed_active
    }
    /// Detach one exact checkout/ref pair while recording the instant of the
    /// user action. `active` guards against deleting a newer association that
    /// arrived between rendering and the click.
    pub(crate) fn detach_association(
        &mut self,
        active: &BranchAssociation,
        tombstone: BranchAssociation,
    ) -> bool {
        let key = association_key_for(active);
        if self.associations.get(&key) != Some(active) {
            return false;
        }
        self.associations.remove(&key);
        self.association_dirty.remove(&key);
        let update_tombstone = self
            .removed_associations
            .get(&key)
            .is_none_or(|existing| association_is_newer(existing, &tombstone));
        if update_tombstone {
            self.removed_associations.insert(key.clone(), tombstone);
        }
        self.removed_association_dirty.insert(key);
        true
    }
    /// Refreshes only verified provenance. Unsaved bookmark choices remain
    /// local to this view, and no refreshed values are marked dirty.
    pub(crate) fn refresh_associations(&mut self, path: &Path) {
        let provenance = Self::load(&path.with_extension("branches.json"));
        self.associations = provenance.associations;
        self.removed_associations = provenance.removed_associations;
        self.history_usage_backfilled = provenance.history_usage_backfilled;
        self.association_dirty.clear();
        self.removed_association_dirty.clear();
        self.history_usage_backfilled_dirty = false;
    }
    pub(crate) fn selected_repo(&self) -> Option<&str> {
        self.selected_repo.as_deref()
    }
    pub(crate) fn set_selected_repo(&mut self, identity: Option<String>) {
        self.selected_repo = identity;
        self.selected_dirty = true;
    }
    pub(crate) fn save(&mut self, path: &Path) -> Result<(), String> {
        self.save_with_lock(path, false).map(|_| ())
    }
    /// Bookmark writes must not replace provenance loaded from its sidecar.
    pub(crate) fn save_bookmarks(&mut self, path: &Path) -> Result<(), String> {
        let associations = std::mem::take(&mut self.associations);
        let removed_associations = std::mem::take(&mut self.removed_associations);
        let association_dirty = std::mem::take(&mut self.association_dirty);
        let removed_association_dirty = std::mem::take(&mut self.removed_association_dirty);
        let history_usage_backfilled = std::mem::take(&mut self.history_usage_backfilled);
        let history_usage_backfilled_dirty =
            std::mem::take(&mut self.history_usage_backfilled_dirty);
        let result = self.save(path);
        self.associations = associations;
        self.removed_associations = removed_associations;
        self.association_dirty = association_dirty;
        self.removed_association_dirty = removed_association_dirty;
        self.history_usage_backfilled = history_usage_backfilled;
        self.history_usage_backfilled_dirty = history_usage_backfilled_dirty;
        result
    }
    /// Persists while waiting for the cross-process lock. Call only from a
    /// blocking background task so an observer is not dropped when another
    /// view is saving preferences.
    pub(crate) fn save_waiting(&mut self, path: &Path) -> Result<(), String> {
        self.save_with_lock(path, true).map(|_| ())
    }
    /// Returns whether this writer changed the history recovery marker from
    /// false to true while holding the preferences lock.
    fn save_with_lock(&mut self, path: &Path, wait_for_lock: bool) -> Result<bool, String> {
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
        let claimed_history_recovery = self.history_usage_backfilled_dirty
            && self.history_usage_backfilled
            && !merged.history_usage_backfilled;
        for key in &self.dirty {
            if let Some(value) = self.repos.get(key) {
                merged.repos.insert(key.clone(), value.clone());
            }
        }
        if self.selected_dirty {
            merged.selected_repo = self.selected_repo.clone();
        }
        // This is deliberately an OR merge. A completed recovery pass must
        // survive a concurrent stale writer and must never be reset by a
        // bookmark-only save.
        if self.history_usage_backfilled_dirty {
            merged.history_usage_backfilled |= self.history_usage_backfilled;
        }
        // First merge and apply removals. A removal deletes only the exact
        // observed evidence; a concurrent newer use is kept.
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
            if merged.associations.get(key).is_some_and(|active| {
                same_evidence(active, removed) || association_is_newer(active, removed)
            }) {
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
                let replacement = match merged.associations.get(key) {
                    Some(existing) if value.inferred && !existing.inferred => None,
                    Some(existing)
                        if !value.inferred
                            && existing.inferred
                            && existing.archived
                            && !association_is_newer(existing, value)
                            && !same_evidence(existing, value) =>
                    {
                        None
                    }
                    Some(existing) if !value.inferred && existing.inferred => {
                        Some(merge_association(existing, value))
                    }
                    Some(existing)
                        if association_is_newer(existing, value)
                            || (same_evidence(existing, value)
                                && !(existing.archived && !value.archived)) =>
                    {
                        Some(merge_association(existing, value))
                    }
                    None => Some(value.clone()),
                    _ => None,
                };
                if let Some(replacement) = replacement {
                    merged.associations.insert(key.clone(), replacement);
                }
            }
        }
        if merged.repos.len() > 1024 {
            return Err("Too many repository bookmarks for this chat.".into());
        }
        if merged.associations.len() > 1024 {
            return Err("Too many verified branch associations for this chat.".into());
        }
        if merged.removed_associations.len() > 1024 {
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
        self.history_usage_backfilled = merged.history_usage_backfilled;
        self.dirty.clear();
        self.selected_dirty = false;
        self.association_dirty.clear();
        self.removed_association_dirty.clear();
        self.history_usage_backfilled_dirty = false;
        Ok(claimed_history_recovery)
    }

    /// Older releases keyed provenance by checkout root. Re-keying from the
    /// values preserves every legacy record and lets new writers coexist with
    /// them without relying on path delimiters.
    fn migrate_association_keys(&mut self) {
        self.associations = migrate_association_map(std::mem::take(&mut self.associations));
        self.removed_associations =
            migrate_association_map(std::mem::take(&mut self.removed_associations));
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

fn same_evidence(left: &BranchAssociation, right: &BranchAssociation) -> bool {
    left.completed_at_ms == right.completed_at_ms && left.evidence_id == right.evidence_id
}

/// Newer clients may know less than a prior record (for example an observer
/// that cannot resolve a historical ref). Keep verified snapshot IDs unless the
/// incoming event supplies a replacement. Equal evidence cannot reactivate
/// archived history; a strictly newer event is handled by its replacement.
fn merge_association(
    existing: &BranchAssociation,
    candidate: &BranchAssociation,
) -> BranchAssociation {
    let mut merged = candidate.clone();
    if merged.last_head.is_none() {
        merged.last_head.clone_from(&existing.last_head);
    }
    if merged.last_base.is_none() {
        merged.last_base.clone_from(&existing.last_base);
    }
    if same_evidence(existing, candidate) {
        merged.archived |= existing.archived;
    }
    merged
}

fn association_order(left: &BranchAssociation, right: &BranchAssociation) -> std::cmp::Ordering {
    match (left.completed_at_ms, right.completed_at_ms) {
        (Some(left_ms), Some(right_ms)) => left_ms
            .cmp(&right_ms)
            .then_with(|| left.evidence_id.cmp(&right.evidence_id)),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => left.evidence_id.cmp(&right.evidence_id),
    }
}

/// JSON encoding makes the persisted composite key unambiguous even when a
/// path or ref contains punctuation that a delimiter-based format cannot use.
fn association_key(repo_root: &str, branch_ref: &str) -> String {
    serde_json::json!([repo_root, branch_ref]).to_string()
}

fn association_key_for(association: &BranchAssociation) -> String {
    association_key(
        &association.repo_root.to_string_lossy(),
        &association.branch_ref,
    )
}

fn migrate_association_map(
    associations: BTreeMap<String, BranchAssociation>,
) -> BTreeMap<String, BranchAssociation> {
    let mut migrated = BTreeMap::new();
    for association in associations.into_values() {
        let key = association_key_for(&association);
        match migrated.get(&key) {
            Some(existing) if !association_is_newer(existing, &association) => {}
            _ => {
                migrated.insert(key, association);
            }
        }
    }
    migrated
}

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;
