//! The wall clock the health system reads, in one place.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub(crate) fn now_secs() -> u64 {
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|e| {
            tracing::warn!("SystemTime before UNIX_EPOCH: {e}");
            Duration::ZERO
        });
    wall.as_secs() + virtual_offset_secs()
}

/// Under tokio's paused time, timers advance without the wall clock moving, so a
/// test could never reach a deadline the machine measures in wall seconds. Tests
/// add how far the paused clock has run ahead of real time. In a runtime that is
/// not paused this is zero.
///
/// It exists in this crate's own tests, and in the tests of a crate that depends on
/// it and turns on the `virtual-time` feature in its dev-dependencies. A release
/// build has neither, so it reads the wall clock and nothing else.
#[cfg(any(test, feature = "virtual-time"))]
fn virtual_offset_secs() -> u64 {
    tokio::time::Instant::now()
        .into_std()
        .saturating_duration_since(std::time::Instant::now())
        .as_secs()
}

#[cfg(not(any(test, feature = "virtual-time")))]
fn virtual_offset_secs() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_reads_wall_time_outside_a_runtime() {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ours = now_secs();
        assert!(ours >= wall && ours - wall <= 2, "{ours} vs {wall}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_clock_follows_paused_time() {
        let before = now_secs();
        tokio::time::sleep(Duration::from_secs(500)).await;
        let after = now_secs();
        assert!(
            (499..=501).contains(&(after - before)),
            "the clock moved {}s",
            after - before
        );
    }

    #[tokio::test]
    async fn the_clock_does_not_run_ahead_when_time_is_not_paused() {
        let before = now_secs();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(now_secs() - before <= 1);
    }
}
