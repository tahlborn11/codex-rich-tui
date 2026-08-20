use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn selection_round_trips_for_each_thread() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let thread_id = ThreadId::new();
    assert_eq!(load(codex_home.path(), thread_id).await.unwrap(), None);

    persist(codex_home.path(), thread_id, AutoSelection::Auto)
        .await
        .unwrap();
    assert_eq!(
        load(codex_home.path(), thread_id).await.unwrap(),
        Some(AutoSelection::Auto)
    );

    persist(codex_home.path(), thread_id, AutoSelection::Manual)
        .await
        .unwrap();
    assert_eq!(
        load(codex_home.path(), thread_id).await.unwrap(),
        Some(AutoSelection::Manual)
    );
}
