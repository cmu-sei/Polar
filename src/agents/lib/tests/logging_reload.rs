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
