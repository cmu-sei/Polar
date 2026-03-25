use polar_scheduler_observer::config::ObserverConfig;
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn test_defaults_when_no_env() {
    let config = ObserverConfig::from_lookup(|_| None);
    assert_eq!(config.local_path, PathBuf::from("/tmp/polar-schedules"));
    assert!(config.remote_url.is_none());
    assert!(config.sync_interval.is_none());
    assert!(config.git_username.is_none());
    assert!(config.git_password.is_none());
}

#[test]
fn test_local_path_override() {
    let config = ObserverConfig::from_lookup(|key| match key {
        "POLAR_SCHEDULER_LOCAL_PATH" => Some("/custom/path".to_string()),
        _ => None,
    });
    assert_eq!(config.local_path, PathBuf::from("/custom/path"));
}

#[test]
fn test_remote_url() {
    let config = ObserverConfig::from_lookup(|key| match key {
        "POLAR_SCHEDULER_REMOTE_URL" => Some("https://github.com/example/repo".to_string()),
        _ => None,
    });
    assert_eq!(
        config.remote_url,
        Some("https://github.com/example/repo".to_string())
    );
}

#[test]
fn test_sync_interval_valid() {
    let config = ObserverConfig::from_lookup(|key| match key {
        "POLAR_SCHEDULER_SYNC_INTERVAL" => Some("30".to_string()),
        _ => None,
    });
    assert_eq!(config.sync_interval, Some(Duration::from_secs(30)));
}

#[test]
fn test_sync_interval_invalid_ignored() {
    let config = ObserverConfig::from_lookup(|key| match key {
        "POLAR_SCHEDULER_SYNC_INTERVAL" => Some("not-a-number".to_string()),
        _ => None,
    });
    assert!(config.sync_interval.is_none());
}

#[test]
fn test_sync_interval_zero() {
    let config = ObserverConfig::from_lookup(|key| match key {
        "POLAR_SCHEDULER_SYNC_INTERVAL" => Some("0".to_string()),
        _ => None,
    });
    assert_eq!(config.sync_interval, Some(Duration::from_secs(0)));
}

#[test]
fn test_git_credentials() {
    let config = ObserverConfig::from_lookup(|key| match key {
        "POLAR_SCHEDULER_GIT_USERNAME" => Some("user".to_string()),
        "POLAR_SCHEDULER_GIT_PASSWORD" => Some("secret".to_string()),
        _ => None,
    });
    assert_eq!(config.git_username, Some("user".to_string()));
    assert_eq!(config.git_password, Some("secret".to_string()));
}

#[test]
fn test_all_vars_set() {
    let config = ObserverConfig::from_lookup(|key| match key {
        "POLAR_SCHEDULER_LOCAL_PATH" => Some("/data/schedules".to_string()),
        "POLAR_SCHEDULER_REMOTE_URL" => Some("file:///tmp/test-repo".to_string()),
        "POLAR_SCHEDULER_SYNC_INTERVAL" => Some("60".to_string()),
        "POLAR_SCHEDULER_GIT_USERNAME" => Some("user".to_string()),
        "POLAR_SCHEDULER_GIT_PASSWORD" => Some("pass".to_string()),
        _ => None,
    });
    assert_eq!(config.local_path, PathBuf::from("/data/schedules"));
    assert_eq!(config.remote_url, Some("file:///tmp/test-repo".to_string()));
    assert_eq!(config.sync_interval, Some(Duration::from_secs(60)));
    assert_eq!(config.git_username, Some("user".to_string()));
    assert_eq!(config.git_password, Some("pass".to_string()));
}
