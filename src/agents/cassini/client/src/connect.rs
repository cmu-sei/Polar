//! Connecting to the broker: how many tries, how long each may take, and how long to wait between.
//!
//! One attempt has to be bounded. While the broker pod is gone its service has no endpoints, so
//! the connection request is dropped rather than refused, and an attempt with no limit waits on
//! the kernel's retransmits, minutes if need be. The agent then finds the broker whenever the
//! kernel next asks, which has nothing to do with when the broker came back. With a limit, every
//! try is a fresh request.

use std::future::Future;
use std::time::Duration;
use tracing::debug;

pub(crate) struct Policy {
    pub attempts: u32,
    pub first_backoff: Duration,
    pub max_backoff: Duration,
    /// The longest one attempt, TCP and TLS together, may take.
    pub attempt_timeout: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            attempts: 8,
            first_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(2),
            attempt_timeout: Duration::from_secs(5),
        }
    }
}

/// Runs `attempt` until it succeeds or the policy runs out, bounding each try. `attempt` is given
/// the number of the try, from 1.
pub(crate) async fn with_retries<T, F, Fut>(policy: &Policy, mut attempt: F) -> Result<T, String>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let mut backoff = policy.first_backoff;
    let mut last_error = String::from("no attempt was made");
    for number in 1..=policy.attempts {
        match tokio::time::timeout(policy.attempt_timeout, attempt(number)).await {
            Ok(Ok(connected)) => return Ok(connected),
            Ok(Err(e)) => {
                debug!(attempt = number, error = %e, "connecting failed");
                last_error = e;
            }
            Err(_) => {
                debug!(attempt = number, timeout = ?policy.attempt_timeout, "connecting timed out");
                last_error = format!("no answer within {:?}", policy.attempt_timeout);
            }
        }
        if number < policy.attempts {
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(policy.max_backoff);
        }
    }
    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::time::Instant;

    fn policy() -> Policy {
        Policy {
            attempts: 4,
            first_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(400),
            attempt_timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_try_that_never_completes_is_given_up_on_and_the_next_one_is_made() {
        let began = Instant::now();
        let tries = Arc::new(AtomicU32::new(0));
        let counted = tries.clone();
        let outcome = with_retries(&policy(), move |n| {
            counted.fetch_add(1, Ordering::SeqCst);
            async move {
                if n <= 2 {
                    std::future::pending::<Result<u32, String>>().await
                } else {
                    Ok(n)
                }
            }
        })
        .await;

        assert_eq!(outcome, Ok(3), "the third try connected");
        assert_eq!(tries.load(Ordering::SeqCst), 3);
        // Two tries each given 5 s, and the waits between: not minutes of waiting on one.
        assert!(
            began.elapsed() < Duration::from_secs(11),
            "{:?}",
            began.elapsed()
        );
        assert!(
            began.elapsed() >= Duration::from_secs(10),
            "{:?}",
            began.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn when_every_try_hangs_it_gives_up_after_the_attempts_in_bounded_time() {
        let began = Instant::now();
        let outcome: Result<u32, String> = with_retries(&policy(), |_| async {
            std::future::pending::<Result<u32, String>>().await
        })
        .await;

        assert!(outcome.is_err());
        // Four tries of 5 s and the waits between them (100, 200, 400 ms).
        assert!(
            began.elapsed() <= Duration::from_secs(21),
            "{:?}",
            began.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_try_that_fails_at_once_is_retried_after_the_backoff_which_doubles_to_its_limit() {
        let began = Instant::now();
        let outcome: Result<u32, String> =
            with_retries(&policy(), |_| async { Err("refused".to_string()) }).await;

        assert_eq!(
            outcome,
            Err("refused".to_string()),
            "the last error is the one reported"
        );
        // Waits of 100, 200 and 400 ms between four tries: none after the last.
        assert_eq!(began.elapsed(), Duration::from_millis(700));
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_try_that_works_ends_it() {
        let began = Instant::now();
        let outcome = with_retries(&policy(), |n| async move { Ok::<_, String>(n) }).await;
        assert_eq!(outcome, Ok(1));
        assert_eq!(began.elapsed(), Duration::ZERO);
    }
}
