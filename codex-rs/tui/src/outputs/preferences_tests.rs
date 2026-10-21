use super::*;

use std::fs;

fn bookmark(target: &str, base: &str) -> RepoBookmark {
    RepoBookmark {
        target: Some(target.into()),
        base: Some(base.into()),
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
