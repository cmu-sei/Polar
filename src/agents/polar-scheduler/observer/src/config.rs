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
    // POLAR_SCHEDULER_LOCAL_PATH
    // POLAR_SCHEDULER_REMOTE_URL
    // POLAR_SCHEDULER_SYNC_INTERVAL
    // POLAR_SCHEDULER_GIT_USERNAME
    // POLAR_SCHEDULER_GIT_PASSWORD
    pub fn from_env() -> Self {
        let local_path = std::env::var("POLAR_SCHEDULER_LOCAL_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp/polar-schedules")); // default

        let remote_url = std::env::var("POLAR_SCHEDULER_REMOTE_URL").ok();

        let sync_interval = std::env::var("POLAR_SCHEDULER_SYNC_INTERVAL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);

        let git_username = std::env::var("POLAR_SCHEDULER_GIT_USERNAME").ok();
        let git_password = std::env::var("POLAR_SCHEDULER_GIT_PASSWORD").ok();

        Self {
            remote_url,
            local_path,
            sync_interval,
            git_username,
            git_password,
        }
    }
}
