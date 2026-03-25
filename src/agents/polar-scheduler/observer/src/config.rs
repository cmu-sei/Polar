use std::path::PathBuf;
use std::time::Duration;

pub struct ObserverConfig {
    pub remote_url: Option<String>,
    pub local_path: PathBuf,
    pub sync_interval: Option<Duration>,
    pub git_username: Option<String>,
    pub git_password: Option<String>,
}

impl ObserverConfig {
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup<F>(lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let local_path = lookup("POLAR_SCHEDULER_LOCAL_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/polar-schedules"));

        let remote_url = lookup("POLAR_SCHEDULER_REMOTE_URL");

        let sync_interval = lookup("POLAR_SCHEDULER_SYNC_INTERVAL")
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);

        let git_username = lookup("POLAR_SCHEDULER_GIT_USERNAME");
        let git_password = lookup("POLAR_SCHEDULER_GIT_PASSWORD");

        Self {
            remote_url,
            local_path,
            sync_interval,
            git_username,
            git_password,
        }
    }
}
