use cassini_tracing::{init_tracing, shutdown_tracing, try_set_parent_otel, try_set_parent_wire};
use cassini_types::WireTraceCtx;
use proptest::prelude::*;

// ===== init_tracing / shutdown_tracing =====

#[test]
fn test_init_tracing_does_not_panic() {
    // May print "Logging registry already initialized" — that's fine.
    init_tracing("test-service");
}

#[test]
fn test_init_tracing_idempotent() {
    init_tracing("test-service-a");
    init_tracing("test-service-b"); // second call must not panic
}

#[test]
fn test_shutdown_tracing_does_not_panic() {
    init_tracing("test-service-shutdown");
    shutdown_tracing();
}

#[test]
fn test_shutdown_tracing_idempotent() {
    shutdown_tracing(); // calling before init is safe
    shutdown_tracing(); // calling twice is safe
}

// ===== try_set_parent_otel =====

#[test]
fn test_try_set_parent_otel_none_is_noop() {
    let span = tracing::info_span!("test-span");
    try_set_parent_otel(&span, None);
}

#[test]
fn test_try_set_parent_otel_with_empty_context() {
    use opentelemetry::Context;
    let span = tracing::info_span!("test-span-ctx");
    let ctx = Context::new();
    try_set_parent_otel(&span, Some(ctx));
}

#[test]
fn test_try_set_parent_otel_outside_span() {
    try_set_parent_otel(&tracing::Span::none(), None);
}

// ===== try_set_parent_wire =====

#[test]
fn test_try_set_parent_wire_none_is_noop() {
    let span = tracing::info_span!("test-wire-span");
    try_set_parent_wire(&span, None);
}

#[test]
fn test_try_set_parent_wire_new_ctx() {
    let span = tracing::info_span!("test-wire-new");
    let ctx = WireTraceCtx::new_for_message();
    try_set_parent_wire(&span, Some(ctx));
}

#[test]
fn test_try_set_parent_wire_child_ctx() {
    let span = tracing::info_span!("test-wire-child");
    let parent = WireTraceCtx::new_for_message();
    let child = parent.create_child();
    try_set_parent_wire(&span, Some(child));
}

#[test]
fn test_try_set_parent_wire_disabled_span() {
    let ctx = WireTraceCtx::new_for_message();
    try_set_parent_wire(&tracing::Span::none(), Some(ctx));
}

#[test]
fn test_try_set_parent_wire_roundtrip() {
    // Create a context, convert to otel context and back — must not panic
    let span = tracing::info_span!("test-wire-roundtrip");
    let ctx = WireTraceCtx::new_for_message();
    let otel_ctx = ctx.to_opentelemetry_context();
    try_set_parent_otel(&span, Some(otel_ctx));
}

// ===== Proptests =====

proptest! {
    #[test]
    fn prop_try_set_parent_wire_new_ctx_no_panic(
        _unused in any::<u8>(),
    ) {
        // Each call generates a fresh random WireTraceCtx — must never panic
        let span = tracing::info_span!("prop-wire-new");
        let ctx = WireTraceCtx::new_for_message();
        try_set_parent_wire(&span, Some(ctx));
    }

    #[test]
    fn prop_try_set_parent_wire_child_no_panic(
        _unused in any::<u8>(),
    ) {
        let span = tracing::info_span!("prop-wire-child");
        let parent = WireTraceCtx::new_for_message();
        let child = parent.create_child();
        try_set_parent_wire(&span, Some(child));
    }

    #[test]
    fn prop_try_set_parent_wire_none_always_noop(
        _unused in any::<u8>(),
    ) {
        let span = tracing::info_span!("prop-wire-none");
        try_set_parent_wire(&span, None);
    }

    #[test]
    fn prop_try_set_parent_otel_none_always_noop(
        _unused in any::<u8>(),
    ) {
        let span = tracing::info_span!("prop-otel-none");
        try_set_parent_otel(&span, None);
    }

    #[test]
    fn prop_otel_context_roundtrip_no_panic(
        _unused in any::<u8>(),
    ) {
        let span = tracing::info_span!("prop-otel-roundtrip");
        let ctx = WireTraceCtx::new_for_message();
        let otel_ctx = ctx.to_opentelemetry_context();
        try_set_parent_otel(&span, Some(otel_ctx));
    }
}
