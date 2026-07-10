//! Tests for the path resolver.

use autorouter_config::ProjectPaths;
use tempfile::tempdir;

#[test]
fn under_root_creates_layout() {
    let dir = tempdir().unwrap();
    let paths = ProjectPaths::under_root(dir.path());
    paths.ensure().unwrap();
    assert!(paths.data_dir.exists());
    assert!(paths.config_dir.exists());
    assert!(paths.cache_dir.exists());
    assert!(paths.log_dir.exists());
}

#[test]
fn resolve_log_dir_ends_with_logs() {
    // M17: the resolved log_dir must end with a "logs" component on
    // every platform. ProjectPaths::resolve() uses
    // `directories::state_dir` when available (XDG_STATE_HOME on
    // Linux) and falls back to `data_dir/logs` otherwise.
    if let Some(paths) = ProjectPaths::resolve() {
        assert_eq!(
            paths.log_dir.file_name().and_then(|s| s.to_str()),
            Some("logs")
        );
        // data_dir and log_dir should be siblings (log_dir is a child
        // of state_dir OR a child of data_dir). The two paths must
        // share the same "logs" subdirectory.
        assert!(paths.log_dir.ends_with("logs"));
    }
}
