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

//! What the broker's bounds hold and have counted, as a log line every half minute while there is something to say,
//! from the pressure manager (`crate::pressure`, `PressureMsg::Report`). The broker has no metrics path of its own
//! (`metrics.rs` is not a module, and `ControlOp::GetBrokerStats` answers that it is not implemented), so the log is
//! where it goes.

use crate::budget::{Budget, BudgetStats};
use std::time::Duration;
use tracing::{debug, info};

/// How often the bounds are reported.
pub const REPORT_EVERY: Duration = Duration::from_secs(30);

/// Reports `budget`: at INFO when anything is held, waits or was counted since `last`, at DEBUG otherwise. Returns
/// what it reported, the next report's `last`.
pub fn report(budget: &Budget, last: Option<&BudgetStats>) -> BudgetStats {
    let now = budget.stats();
    let changed = last.is_none_or(|before| {
        before.refused != now.refused
            || before.slow_disconnections != now.slow_disconnections
            || before.alarms != now.alarms
            || before.charges != now.charges
    });
    if now.used > 0 || now.readers_waiting > 0 || changed {
        info!(
            used = now.used,
            limit = now.limit,
            peak = now.peak,
            exhausted = now.exhausted,
            readers_waiting = now.readers_waiting,
            refused = now.refused,
            slow_disconnections = now.slow_disconnections,
            alarms = now.alarms,
            largest = ?budget.largest_holders(5),
            "the broker's memory budget"
        );
    } else {
        debug!(
            limit = now.limit,
            "the broker's memory budget holds nothing"
        );
    }
    now
}
