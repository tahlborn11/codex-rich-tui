//! Bounded, coalesced branch observations for an app session.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use crate::branch_association::BranchUse;
use crate::branch_association::CreatedBranch;

pub(in crate::app) struct BranchObservers {
    slots: Mutex<BTreeMap<String, Arc<Mutex<Pending>>>>,
    pub(super) permits: Arc<tokio::sync::Semaphore>,
}

impl Default for BranchObservers {
    fn default() -> Self {
        Self {
            slots: Mutex::default(),
            permits: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
}

#[derive(Default)]
pub(super) struct Pending {
    pub(super) verified_cwd: Option<PathBuf>,
    running: bool,
    uses: BTreeMap<PathBuf, BranchUse>,
    creations: Vec<CreatedBranch>,
}

impl BranchObservers {
    /// One worker per thread/connection scope, at most 128 pending locations.
    /// Start/completion events for the same location coalesce while it is busy.
    pub(super) fn enqueue(
        &self,
        scope: String,
        uses: Vec<BranchUse>,
        creations: Vec<CreatedBranch>,
    ) -> Option<Arc<Mutex<Pending>>> {
        // Keep the map locked until the slot is marked running. Otherwise an
        // arriving scope could evict a just-created, not-yet-running worker.
        let mut slots = self.slots.lock().ok()?;
        if !slots.contains_key(&scope) && slots.len() >= 256 {
            slots.retain(|_, slot| slot.lock().is_ok_and(|pending| pending.running));
            if slots.len() >= 256 {
                return None;
            }
        }
        let slot = slots.entry(scope).or_default().clone();
        {
            let mut pending = slot.lock().ok()?;
            for usage in uses {
                if pending.uses.contains_key(&usage.cwd) || pending.uses.len() < 128 {
                    let replace = pending
                        .uses
                        .get(&usage.cwd)
                        .is_none_or(|old| old.completed_at_ms <= usage.completed_at_ms);
                    if replace {
                        pending.uses.insert(usage.cwd.clone(), usage);
                    }
                }
            }
            let remaining = 128_usize.saturating_sub(pending.creations.len());
            pending
                .creations
                .extend(creations.into_iter().take(remaining));
            if pending.running {
                return None;
            }
            pending.running = true;
        }
        Some(slot)
    }
}

impl Pending {
    pub(super) fn take_batch(&mut self) -> Option<(Vec<BranchUse>, Vec<CreatedBranch>)> {
        if self.uses.is_empty() && self.creations.is_empty() {
            self.running = false;
            return None;
        }
        Some((
            std::mem::take(&mut self.uses).into_values().collect(),
            std::mem::take(&mut self.creations),
        ))
    }
    pub(super) fn cancel(&mut self) {
        self.uses.clear();
        self.creations.clear();
        self.running = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_duplicate_locations_and_restarts_after_draining() {
        let observers = BranchObservers::default();
        let usage = |timestamp| BranchUse {
            cwd: "/repo".into(),
            evidence_id: format!("{timestamp}"),
            completed_at_ms: Some(timestamp),
            inferred: false,
        };
        let slot = observers
            .enqueue("thread".into(), vec![usage(1)], vec![])
            .unwrap();
        assert!(
            observers
                .enqueue("thread".into(), vec![usage(2)], vec![])
                .is_none()
        );
        let mut pending = slot.lock().unwrap();
        let (uses, _) = pending.take_batch().unwrap();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].completed_at_ms, Some(2));
        assert!(pending.take_batch().is_none());
        drop(pending);
        assert!(
            observers
                .enqueue("thread".into(), vec![usage(3)], vec![])
                .is_some()
        );
    }
    #[test]
    fn bounds_pending_locations_and_scopes() {
        let observers = BranchObservers::default();
        let uses = (0..200)
            .map(|index| BranchUse {
                cwd: format!("/repo/{index}").into(),
                evidence_id: index.to_string(),
                completed_at_ms: Some(index),
                inferred: false,
            })
            .collect();
        let slot = observers.enqueue("first".into(), uses, vec![]).unwrap();
        assert_eq!(slot.lock().unwrap().take_batch().unwrap().0.len(), 128);
        for index in 1..256 {
            assert!(
                observers
                    .enqueue(index.to_string(), vec![], vec![])
                    .is_some()
            );
        }
        assert!(
            observers
                .enqueue("overflow".into(), vec![], vec![])
                .is_none()
        );
    }
    #[test]
    fn admits_new_chats_after_drained_scopes_are_evicted() {
        let observers = BranchObservers::default();
        for index in 0..256 {
            let slot = observers
                .enqueue(index.to_string(), vec![], vec![])
                .unwrap();
            assert!(slot.lock().unwrap().take_batch().is_none());
        }
        assert!(
            observers
                .enqueue("next-chat".into(), vec![], vec![])
                .is_some()
        );
    }
}
