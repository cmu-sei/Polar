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

//! Timestamp helpers for types that store Unix millis directly (the
//! established convention for anything crossing an rkyv boundary --
//! rkyv has no native chrono support, and every existing type in this
//! codebase that needs a timestamp on such a type already avoids
//! storing chrono::DateTime directly rather than working around that
//! gap per-type). These two functions are the only place chrono
//! should be reached for in that situation: to produce the stored
//! integer, and to render one back into something human-readable.

/// The current time as Unix milliseconds.
pub fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Renders stored Unix millis as an RFC3339 string, for a human
/// reader (a log line, an error message). Returns `None` only for a
/// value so far out of range it can't be represented as a valid
/// `DateTime` at all -- not expected for any real, freshly-produced
/// timestamp.
pub fn format_millis_rfc3339(millis: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(millis).map(|dt| dt.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_millis_is_plausible() {
        let before = chrono::Utc::now().timestamp_millis();
        let now = now_millis();
        let after = chrono::Utc::now().timestamp_millis();
        assert!(now >= before && now <= after);
    }

    #[test]
    fn format_millis_rfc3339_round_trips_a_known_value() {
        // 2026-01-01T00:00:00Z
        let millis = 1767225600000;
        let formatted = format_millis_rfc3339(millis).expect("valid timestamp");
        assert!(formatted.starts_with("2026-01-01T00:00:00"));
    }
}
