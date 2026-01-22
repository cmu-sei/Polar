pub mod actors;

use std::time::{SystemTime, UNIX_EPOCH};

fn get_timestamp_in_milliseconds() -> Result<u128, std::time::SystemTimeError> {
    let current_system_time = SystemTime::now();
    let duration_since_epoch = current_system_time.duration_since(UNIX_EPOCH)?;
    let milliseconds_timestamp = duration_since_epoch.as_millis();
    Ok(milliseconds_timestamp)
}
