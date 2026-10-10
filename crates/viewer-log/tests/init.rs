//! The global setup path; one test per process because the subscriber is global.

use viewer_log::{InitError, LogConfig, RedactedTail, init};

#[test]
fn init_writes_file_and_rejects_second_init() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = LogConfig::new(dir.path(), "app");
    cfg.stderr = false;
    let guard = init(&cfg).unwrap();
    tracing::info!(uid = %RedactedTail("UID-EXAMPLE-0042"), "camera connected");
    tracing::debug!("below the default level");
    assert!(matches!(init(&cfg), Err(InitError::AlreadyInitialized)));
    drop(guard);
    let text = std::fs::read_to_string(dir.path().join("app.log")).unwrap();
    assert!(
        text.contains("camera connected") && text.contains("****0042"),
        "{text}"
    );
    assert!(!text.contains("below the default level") && !text.contains("UID-EXAMPLE"));
}
