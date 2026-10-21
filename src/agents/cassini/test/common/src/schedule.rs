/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! How a producer paces itself. Message `i` is due `i / rate` seconds after the start; the producer wakes, sends every
//! message that is due, and sleeps until the next. A timer cannot wake much more often than once a millisecond, so at
//! high rates each wake sends a small batch, and the rate holds. A rate of 0 is as fast as the producer can go.
//!
//! The producer also never has more than its window of messages sent and not yet acknowledged by the broker. A
//! producer that outruns the broker falls behind its rate, and says so (the rate it reached), instead of holding a
//! run's worth of messages in memory.

use std::time::Duration;

/// When message `i` (from 0) is due, after the start, to the nanosecond at or after it (so that a producer woken then
/// finds it due); 0 for a rate of 0.
pub fn due_at(i: u64, rate: u64) -> Duration {
    if rate == 0 {
        Duration::ZERO
    } else {
        Duration::from_nanos((i as u128 * 1_000_000_000).div_ceil(rate as u128) as u64)
    }
}

/// How many of `total` messages are due by `elapsed` after the start.
pub fn due_by(elapsed: Duration, rate: u64, total: u64) -> u64 {
    if rate == 0 {
        return total;
    }
    let due = (elapsed.as_nanos() * rate as u128) / 1_000_000_000 + 1;
    due.min(total as u128) as u64
}

/// How many messages a window of `bytes` holds, of messages `size` bytes long: at least one.
pub fn window(bytes: u64, size: u64) -> u64 {
    (bytes / size.max(1)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_thousand_a_second_is_one_a_millisecond() {
        assert_eq!(due_at(3, 1000), Duration::from_millis(3));
        assert_eq!(due_by(Duration::ZERO, 1000, 10), 1);
        assert_eq!(due_by(Duration::from_micros(2500), 1000, 10), 3);
        assert_eq!(due_by(Duration::from_secs(60), 1000, 10), 10);
        assert_eq!(due_by(Duration::ZERO, 0, 10), 10);
        assert_eq!(window(64 << 20, 1 << 20), 64);
        assert_eq!(window(64 << 20, 1 << 30), 1);
    }

    proptest! {
        /// What is due never shrinks as time passes, never exceeds the run, and includes every message whose time has
        /// come: woken at message `i`'s time, the producer sends at least up to it.
        #[test]
        fn what_is_due_grows_with_time_and_includes_every_message_whose_time_has_come(
            rate in 0u64..2_000_000,
            total in 0u64..100_000,
            a in 0u64..10_000_000_000,
            b in 0u64..10_000_000_000,
            i in 0u64..100_000,
        ) {
            let (a, b) = (Duration::from_nanos(a.min(b)), Duration::from_nanos(a.max(b)));
            prop_assert!(due_by(a, rate, total) <= due_by(b, rate, total));
            prop_assert!(due_by(b, rate, total) <= total);
            if i < total {
                prop_assert!(due_by(due_at(i, rate), rate, total) > i);
            }
        }
    }
}
