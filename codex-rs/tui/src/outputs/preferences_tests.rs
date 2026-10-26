use super::*;

use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn bookmark(target: &str, base: &str) -> RepoBookmark {
    RepoBookmark {
        target: Some(target.into()),
        base: Some(base.into()),
        target_is_explicit: false,
    }
}

#[test]
fn malformed_or_oversize_preferences_are_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    fs::write(&path, b"not json").unwrap();
    assert!(Preferences::load(&path).bookmark("repo").is_none());

    fs::write(&path, vec![b'x'; MAX_PREFERENCES_BYTES as usize + 1]).unwrap();
    assert!(Preferences::load(&path).bookmark("repo").is_none());
}

#[test]
fn save_creates_parent_and_preserves_distinct_repo_updates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested").join("thread.json");
    let mut first = Preferences::default();
    first.set("repo-a".into(), bookmark("topic-a", "main"));
    first.set_selected_repo(Some("repo-a".into()));
    first.save(&path).unwrap();

    // A second retained view starts from stale in-memory preferences. Save
    // reloads the file beneath its lock, so it does not erase repo-a.
    let mut second = Preferences::default();
    second.set("repo-b".into(), bookmark("topic-b", "main"));
    second.save(&path).unwrap();

    let loaded = Preferences::load(&path);
    assert_eq!(
        loaded.bookmark("repo-a").unwrap().target.as_deref(),
        Some("topic-a")
    );
    assert_eq!(
        loaded.bookmark("repo-b").unwrap().target.as_deref(),
        Some("topic-b")
    );
    assert_eq!(loaded.selected_repo(), Some("repo-a"));
}

#[test]
fn busy_lock_reports_error_and_never_replaces_preferences() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let lock = path.with_extension("lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)
        .unwrap();
    lock_file.try_lock().unwrap();
    let mut preferences = Preferences::default();
    preferences.set("repo".into(), bookmark("topic", "main"));
    assert!(preferences.save(&path).is_err());
    assert!(!path.exists());
}

#[test]
fn failed_persist_releases_the_os_lock() {
    // A directory at the destination makes persist fail. The RAII lock must
    // still be removed so a later attempt is not permanently blocked.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    fs::create_dir(&path).unwrap();
    let mut preferences = Preferences::default();
    assert!(preferences.save(&path).is_err());
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .unwrap();
    assert!(lock_file.try_lock().is_ok());
}

#[test]
fn stale_writer_only_merges_explicitly_changed_repositories() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let mut initial = Preferences::default();
    initial.set("a".into(), bookmark("old", "main"));
    initial.save(&path).unwrap();
    let mut first = Preferences::load(&path);
    let mut second = Preferences::load(&path);
    first.set("a".into(), bookmark("new", "main"));
    first.save(&path).unwrap();
    second.set("b".into(), bookmark("topic", "main"));
    second.save(&path).unwrap();
    assert_eq!(
        Preferences::load(&path)
            .bookmark("a")
            .unwrap()
            .target
            .as_deref(),
        Some("new")
    );
}

fn association(root: &str, branch: &str, evidence_id: &str) -> BranchAssociation {
    BranchAssociation {
        repo_root: root.into(),
        branch_ref: branch.into(),
        evidence_id: evidence_id.into(),
        completed_at_ms: None,
        ..Default::default()
    }
}

#[test]
fn stale_same_repo_writer_cannot_replace_newer_association() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let mut old = Preferences::default();
    assert!(old.record_association(BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/old", "old")
    }));
    old.save(&path).unwrap();

    let mut delayed = Preferences::load(&path);
    let mut current = Preferences::load(&path);
    assert!(current.record_association(BranchAssociation {
        completed_at_ms: Some(20),
        ..association("/repo", "refs/heads/new", "new")
    }));
    current.save(&path).unwrap();
    assert!(delayed.record_association(BranchAssociation {
        completed_at_ms: Some(15),
        ..association("/repo", "refs/heads/stale", "stale")
    }));
    delayed.save(&path).unwrap();

    let stored = Preferences::load(&path)
        .association("/repo")
        .unwrap()
        .clone();
    assert_eq!(stored.branch_ref, "refs/heads/new");
    assert_eq!(stored.evidence_id, "new");
}

#[test]
fn equal_timestamps_choose_the_same_evidence_id_winner_in_both_orders() {
    for order in [["event-a", "event-b"], ["event-b", "event-a"]] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thread.json");
        let mut first = Preferences::default();
        assert!(first.record_association(BranchAssociation {
            completed_at_ms: Some(10),
            ..association("/repo", &format!("refs/heads/{}", order[0]), order[0])
        }));
        first.save(&path).unwrap();

        let mut second = Preferences::load(&path);
        let _ = second.record_association(BranchAssociation {
            completed_at_ms: Some(10),
            ..association("/repo", &format!("refs/heads/{}", order[1]), order[1])
        });
        second.save(&path).unwrap();
        let winner = Preferences::load(&path)
            .association("/repo")
            .unwrap()
            .clone();
        assert_eq!(winner.evidence_id, "event-b");
        assert_eq!(winner.branch_ref, "refs/heads/event-b");
    }
}

#[test]
fn known_timestamp_replaces_unknown_but_unknown_cannot_replace_known() {
    let mut preferences = Preferences::default();
    assert!(preferences.record_association(association("/repo", "refs/heads/topic", "old")));
    assert!(preferences.record_association(BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/topic", "new")
    }));
    assert!(!preferences.record_association(association("/repo", "refs/heads/topic", "later")));
    assert_eq!(preferences.association("/repo").unwrap().evidence_id, "new");
}

#[test]
fn waiting_saves_merge_independent_associations_held_by_an_os_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = Arc::new(directory.path().join("thread.json"));
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .unwrap();
    lock_file.try_lock().unwrap();

    let writers = ["/repo/a", "/repo/b"].map(|root| {
        let path = Arc::clone(&path);
        thread::spawn(move || {
            let mut preferences = Preferences::default();
            assert!(preferences.record_association(association(root, "refs/heads/topic", root)));
            preferences.save_waiting(&path).unwrap();
        })
    });
    // Both background saves are invoked while the lock is held. They wait and
    // merge in turn after it is released instead of dropping either update.
    thread::sleep(Duration::from_millis(20));
    drop(lock_file);
    for writer in writers {
        writer.join().unwrap();
    }

    let loaded = Preferences::load(&path);
    assert!(loaded.association("/repo/a").is_some());
    assert!(loaded.association("/repo/b").is_some());
}

#[test]
fn associations_survive_restart_without_promoting_bookmarks_to_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let mut preferences = Preferences::default();
    preferences.set("/repo/manual".into(), bookmark("topic", "main"));
    assert!(preferences.record_association(association(
        "/repo/verified",
        "refs/heads/topic",
        "call-1"
    )));
    assert!(!preferences.record_association(association(
        "/repo/verified",
        "refs/heads/topic",
        "call-1"
    )));
    preferences.save(&path).unwrap();

    let loaded = Preferences::load(&path);
    assert!(loaded.association("/repo/manual").is_none());
    assert_eq!(
        loaded.association("/repo/verified").unwrap().branch_ref,
        "refs/heads/topic"
    );
}

#[test]
fn stale_writer_merges_associations_without_losing_bookmarks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let mut first = Preferences::default();
    first.set("manual-a".into(), bookmark("topic-a", "main"));
    assert!(first.record_association(association("/repo/a", "refs/heads/a", "call-a")));
    first.save(&path).unwrap();

    let mut stale = Preferences::load(&path);
    let mut newer = Preferences::load(&path);
    newer.set("manual-b".into(), bookmark("topic-b", "main"));
    assert!(newer.record_association(association("/repo/b", "refs/heads/b", "call-b")));
    newer.save(&path).unwrap();
    assert!(stale.record_association(association("/repo/c", "refs/heads/c", "call-c")));
    stale.save(&path).unwrap();

    let loaded = Preferences::load(&path);
    assert!(loaded.bookmark("manual-a").is_some());
    assert!(loaded.bookmark("manual-b").is_some());
    assert!(loaded.association("/repo/a").is_some());
    assert!(loaded.association("/repo/b").is_some());
    assert!(loaded.association("/repo/c").is_some());
}

#[test]
fn refresh_associations_keeps_unsaved_bookmarks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let mut saved = Preferences::default();
    assert!(saved.record_association(association("/repo/a", "refs/heads/a", "call-a")));
    saved.save(&path.with_extension("branches.json")).unwrap();

    let mut view = Preferences::default();
    view.set("manual".into(), bookmark("topic", "main"));
    view.refresh_associations(&path);
    assert!(view.bookmark("manual").is_some());
    assert!(view.association("/repo/a").is_some());
    view.save_bookmarks(&path).unwrap();
    assert!(view.association("/repo/a").is_some());
    assert!(
        Preferences::load_with_associations(&path)
            .association("/repo/a")
            .is_some()
    );
}

#[test]
fn removed_association_survives_reload_and_does_not_erase_a_bookmark() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let sidecar = path.with_extension("branches.json");
    let owned = BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo/a", "refs/heads/topic", "call-a")
    };
    let mut preferences = Preferences::default();
    preferences.set("manual".into(), bookmark("topic", "main"));
    preferences.save_bookmarks(&path).unwrap();
    assert!(preferences.record_association(owned.clone()));
    preferences.save_waiting(&sidecar).unwrap();

    let mut loaded = Preferences::load_with_associations(&path);
    assert!(loaded.remove_association(&owned));
    loaded.save_waiting(&sidecar).unwrap();
    loaded.save_bookmarks(&path).unwrap();

    let reloaded = Preferences::load_with_associations(&path);
    assert!(reloaded.association("/repo/a").is_none());
    assert!(
        reloaded
            .removed_associations
            .contains_key(&association_key("/repo/a", "refs/heads/topic"))
    );
    assert!(reloaded.bookmark("manual").is_some());
}

#[test]
fn tombstone_blocks_stale_persistence_but_allows_a_strictly_newer_creation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let sidecar = path.with_extension("branches.json");
    let old = BranchAssociation {
        completed_at_ms: Some(20),
        ..association("/repo", "refs/heads/old", "call-old")
    };
    let mut initial = Preferences::default();
    assert!(initial.record_association(old.clone()));
    initial.save_waiting(&sidecar).unwrap();

    // This writer acquired old history before the deletion, but its evidence
    // is older than the removal and must not resurrect the association.
    let mut stale = Preferences::default();
    assert!(stale.record_association(BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/old", "call-stale")
    }));
    let mut remover = Preferences::load_with_associations(&path);
    assert!(remover.remove_association(&old));
    remover.save_waiting(&sidecar).unwrap();
    stale.save_waiting(&sidecar).unwrap();
    assert!(
        Preferences::load_with_associations(&path)
            .association("/repo")
            .is_none()
    );

    let mut newer = Preferences::load_with_associations(&path);
    assert!(newer.record_association(BranchAssociation {
        completed_at_ms: Some(30),
        ..association("/repo", "refs/heads/new", "call-new")
    }));
    newer.save_waiting(&sidecar).unwrap();
    assert_eq!(
        Preferences::load_with_associations(&path)
            .association("/repo")
            .unwrap()
            .branch_ref,
        "refs/heads/new"
    );
}

#[test]
fn stale_removal_records_tombstone_without_deleting_newer_association() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let sidecar = path.with_extension("branches.json");
    let old = BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/old", "call-old")
    };
    let new = BranchAssociation {
        completed_at_ms: Some(20),
        ..association("/repo", "refs/heads/new", "call-new")
    };
    let mut first = Preferences::default();
    assert!(first.record_association(old.clone()));
    first.save_waiting(&sidecar).unwrap();
    let mut stale_remover = Preferences::load_with_associations(&path);

    let mut current = Preferences::load_with_associations(&path);
    assert!(current.record_association(new.clone()));
    current.save_waiting(&sidecar).unwrap();

    assert!(stale_remover.remove_association(&old));
    stale_remover.save_waiting(&sidecar).unwrap();
    let reloaded = Preferences::load_with_associations(&path);
    assert_eq!(
        reloaded.association_for("/repo", "refs/heads/new"),
        Some(&new)
    );
    assert!(
        reloaded
            .association_for("/repo", "refs/heads/old")
            .is_none()
    );
    assert_eq!(
        reloaded
            .removed_associations
            .get(&association_key("/repo", "refs/heads/old")),
        Some(&old)
    );
}

#[test]
fn multiple_branches_in_one_checkout_are_retained_and_sorted_by_use() {
    let mut preferences = Preferences::default();
    let older = BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/older", "old")
    };
    let newer = BranchAssociation {
        completed_at_ms: Some(20),
        ..association("/repo", "refs/heads/newer", "new")
    };
    assert!(preferences.record_association(older.clone()));
    assert!(preferences.record_association(newer.clone()));
    assert_eq!(preferences.association("/repo"), Some(&newer));
    assert_eq!(
        preferences
            .associations_for("/repo")
            .into_iter()
            .map(|association| association.branch_ref.as_str())
            .collect::<Vec<_>>(),
        vec!["refs/heads/newer", "refs/heads/older"]
    );
    assert_eq!(
        preferences.association_for("/repo", "refs/heads/older"),
        Some(&older)
    );
}

#[test]
fn legacy_root_keyed_associations_migrate_without_losing_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.branches.json");
    let legacy = association("/repo", "refs/heads/topic", "old-client");
    let bytes = serde_json::json!({"associations": {"/repo": legacy}});
    fs::write(&path, serde_json::to_vec(&bytes).unwrap()).unwrap();
    let loaded = Preferences::load(&path);
    assert_eq!(
        loaded
            .association_for("/repo", "refs/heads/topic")
            .unwrap()
            .evidence_id,
        "old-client"
    );
    assert!(!loaded.associations.contains_key("/repo"));
}

#[test]
fn stale_writers_merge_distinct_branches_in_the_same_checkout() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let mut first = Preferences::default();
    let mut second = Preferences::default();
    assert!(first.record_association(association("/repo", "refs/heads/a", "a")));
    assert!(second.record_association(association("/repo", "refs/heads/b", "b")));
    first.save_waiting(&path).unwrap();
    second.save_waiting(&path).unwrap();
    let loaded = Preferences::load(&path);
    assert!(loaded.association_for("/repo", "refs/heads/a").is_some());
    assert!(loaded.association_for("/repo", "refs/heads/b").is_some());
}

#[test]
fn separate_chat_sidecars_can_associate_the_same_branch() {
    let directory = tempfile::tempdir().unwrap();
    let association = association("/repo", "refs/heads/topic", "event");
    for name in ["chat-a.json", "chat-b.json"] {
        let path = directory.path().join(name);
        let mut preferences = Preferences::default();
        assert!(preferences.record_association(association.clone()));
        preferences.save_waiting(&path).unwrap();
    }
    for name in ["chat-a.json", "chat-b.json"] {
        assert!(
            Preferences::load(&directory.path().join(name))
                .association_for("/repo", "refs/heads/topic")
                .is_some()
        );
    }
}

#[test]
fn explicit_detach_is_scoped_to_one_branch() {
    let mut preferences = Preferences::default();
    let left = BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/left", "left")
    };
    let right = BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/right", "right")
    };
    preferences.record_association(left.clone());
    preferences.record_association(right.clone());
    assert!(preferences.remove_association(&left));
    assert!(
        preferences
            .association_for("/repo", "refs/heads/left")
            .is_none()
    );
    assert_eq!(
        preferences.association_for("/repo", "refs/heads/right"),
        Some(&right)
    );
}

#[test]
fn detach_tombstone_blocks_delayed_same_ref_observation_but_allows_later_use() {
    let mut preferences = Preferences::default();
    let visible = BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/topic", "usage-visible")
    };
    assert!(preferences.record_association(visible.clone()));
    // The click happens after a command was emitted but before that command's
    // delayed observer has reached the sidecar.
    let detached = BranchAssociation {
        completed_at_ms: Some(30),
        evidence_id: "detach:30:0".into(),
        ..visible.clone()
    };
    assert!(preferences.detach_association(&visible, detached));
    assert!(!preferences.record_association(BranchAssociation {
        completed_at_ms: Some(20),
        ..association("/repo", "refs/heads/topic", "usage-delayed")
    }));
    assert!(preferences.record_association(BranchAssociation {
        completed_at_ms: Some(40),
        ..association("/repo", "refs/heads/topic", "usage-later")
    }));
    assert_eq!(
        preferences
            .association_for("/repo", "refs/heads/topic")
            .unwrap()
            .evidence_id,
        "usage-later"
    );
}

#[test]
fn history_recovery_marker_is_sidecar_durable_and_bookmark_saves_cannot_reset_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.json");
    let sidecar = path.with_extension("branches.json");
    let mut recovered = Preferences::default();
    recovered.mark_history_usage_backfilled();
    recovered.save_waiting(&sidecar).unwrap();

    let mut bookmarks = Preferences::load_with_associations(&path);
    bookmarks.set("/repo".into(), bookmark("topic", "main"));
    bookmarks.save_bookmarks(&path).unwrap();

    assert!(Preferences::load_with_associations(&path).history_usage_backfilled());
    assert!(!Preferences::load(&path).history_usage_backfilled());
}

#[test]
fn history_recovery_marker_merges_monotonically_with_a_stale_writer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.branches.json");
    let mut stale = Preferences::default();
    stale.set("manual".into(), bookmark("topic", "main"));
    let mut recovered = Preferences::default();
    recovered.mark_history_usage_backfilled();
    recovered.save_waiting(&path).unwrap();
    stale.save_waiting(&path).unwrap();
    assert!(Preferences::load(&path).history_usage_backfilled());
}

#[test]
fn history_recovery_claim_is_atomic_for_concurrent_views() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.branches.json");
    let mut first = Preferences::default();
    let mut second = Preferences::default();
    assert!(first.claim_history_usage_backfilled(&path).unwrap());
    assert!(!second.claim_history_usage_backfilled(&path).unwrap());
    assert!(Preferences::load(&path).history_usage_backfilled());
}

#[test]
fn archival_metadata_stays_visible_and_cannot_be_resurrected_by_equal_evidence() {
    let mut preferences = Preferences::default();
    let active = BranchAssociation {
        completed_at_ms: Some(10),
        last_head: Some("a".into()),
        ..association("/repo", "refs/heads/topic", "event")
    };
    let archived = BranchAssociation {
        archived: true,
        last_base: Some("b".into()),
        ..active.clone()
    };
    assert!(preferences.record_association(active.clone()));
    assert!(preferences.record_association(archived.clone()));
    assert!(!preferences.record_association(active));
    assert_eq!(
        preferences.association_for("/repo", "refs/heads/topic"),
        Some(&archived)
    );
}

#[test]
fn inferred_evidence_cannot_replace_precise_evidence_but_precise_can_upgrade_it() {
    let mut preferences = Preferences::default();
    let precise = BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/topic", "precise")
    };
    let inferred = BranchAssociation {
        completed_at_ms: Some(20),
        inferred: true,
        ..association("/repo", "refs/heads/topic", "recovered")
    };
    preferences.record_association(precise.clone());
    assert!(!preferences.record_association(inferred));
    assert_eq!(
        preferences.association_for("/repo", "refs/heads/topic"),
        Some(&precise)
    );

    let mut recovered = Preferences::default();
    let inferred = BranchAssociation {
        inferred: true,
        ..association("/repo", "refs/heads/topic", "recovered")
    };
    let direct = BranchAssociation {
        completed_at_ms: Some(1),
        ..association("/repo", "refs/heads/topic", "direct")
    };
    recovered.record_association(inferred);
    assert!(recovered.record_association(direct.clone()));
    assert_eq!(
        recovered.association_for("/repo", "refs/heads/topic"),
        Some(&direct)
    );
}

#[test]
fn older_precise_evidence_cannot_unarchive_inferred_history() {
    let mut preferences = Preferences::default();
    let inferred_archived = BranchAssociation {
        completed_at_ms: Some(20),
        inferred: true,
        archived: true,
        ..association("/repo", "refs/heads/topic", "usage:recovered")
    };
    preferences.record_association(inferred_archived.clone());
    let old_creation = BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/topic", "creation:old")
    };
    assert!(!preferences.record_association(old_creation));
    assert_eq!(
        preferences.association_for("/repo", "refs/heads/topic"),
        Some(&inferred_archived)
    );
}

#[test]
fn stale_precise_writer_cannot_unarchive_newer_inferred_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("branches.json");
    let mut stale = Preferences::default();
    stale.record_association(BranchAssociation {
        completed_at_ms: Some(10),
        ..association("/repo", "refs/heads/topic", "old-creation")
    });
    let archived = BranchAssociation {
        completed_at_ms: Some(20),
        inferred: true,
        archived: true,
        ..association("/repo", "refs/heads/topic", "newer-history")
    };
    let mut current = Preferences::default();
    current.record_association(archived.clone());
    current.save_waiting(&path).unwrap();
    stale.save_waiting(&path).unwrap();
    assert_eq!(
        Preferences::load(&path).association_for("/repo", "refs/heads/topic"),
        Some(&archived)
    );
}
