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

//! The log filters can be changed while the process runs. One test, in a file of its own: the
//! subscriber is global to the process, so nothing else may set it up first.

use polar::logging::{LoggingSettings, init_with, reload};
use tracing::Level;

fn settings(console: &str) -> LoggingSettings {
    LoggingSettings {
        console_filter: console.to_string(),
        trace_filter: "info".to_string(),
        otlp_endpoint: None,
    }
}

#[test]
fn the_console_filter_is_changed_in_place_and_a_bad_one_changes_nothing() {
    init_with("reload-test".to_string(), &settings("warn"));
    assert!(!tracing::enabled!(Level::DEBUG), "warn is the filter");
    assert!(!tracing::enabled!(Level::INFO));

    reload(&settings("debug")).expect("a filter that parses is applied");
    assert!(tracing::enabled!(Level::DEBUG), "debug is the filter now");

    let refused = reload(&settings("not=a=filter["));
    assert!(refused.is_err());
    assert!(
        tracing::enabled!(Level::DEBUG),
        "the filter in use is the last good one"
    );

    reload(&settings("error")).unwrap();
    assert!(!tracing::enabled!(Level::WARN), "back to quiet");
}
