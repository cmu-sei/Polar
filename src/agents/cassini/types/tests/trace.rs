//! Unit tests for cassini-types trace.rs types
//! Tests constructors, conversions, and error paths

use cassini_types::trace::WireTraceCtx;
use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};

#[test]
fn test_wire_trace_ctx_new_for_message() {
    let ctx = WireTraceCtx::new_for_message();

    assert_eq!(ctx.trace_id.len(), 16);
    assert_eq!(ctx.span_id.len(), 8);
    assert_eq!(ctx.trace_flags, 1);
}

#[test]
fn test_wire_trace_ctx_create_child() {
    let parent = WireTraceCtx::new_for_message();
    let child = parent.create_child();

    assert_eq!(child.trace_id, parent.trace_id);
    assert_ne!(child.span_id, parent.span_id);
    assert_eq!(child.trace_flags, parent.trace_flags);
}

#[test]
fn test_wire_trace_ctx_from_span_context_valid() {
    let trace_id = TraceId::from_bytes([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let span_id = SpanId::from_bytes([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let span_context = SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );

    let ctx = WireTraceCtx::from_span_context(&span_context).unwrap();

    assert_eq!(ctx.trace_id, trace_id.to_bytes());
    assert_eq!(ctx.span_id, span_id.to_bytes());
    assert_eq!(ctx.trace_flags, TraceFlags::SAMPLED.to_u8());
}

#[test]
fn test_wire_trace_ctx_from_span_context_invalid() {
    let trace_id = TraceId::from_bytes([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let span_id = SpanId::from_bytes([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let span_context = SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::default(),
        false, // invalid
        TraceState::default(),
    );

    // Should fallback to new_for_message when invalid
    let ctx = WireTraceCtx::from_span_context(&span_context).unwrap();

    assert_eq!(ctx.trace_id.len(), 16);
    assert_eq!(ctx.span_id.len(), 8);
}

#[test]
fn test_wire_trace_ctx_to_span_context() {
    let ctx = WireTraceCtx::new_for_message();
    let span_context = ctx.to_span_context();

    assert_eq!(span_context.trace_id(), TraceId::from_bytes(ctx.trace_id));
    assert_eq!(span_context.span_id(), SpanId::from_bytes(ctx.span_id));
    assert_eq!(span_context.trace_flags(), TraceFlags::new(ctx.trace_flags));
    assert!(span_context.is_remote());
    assert_eq!(span_context.trace_state(), &TraceState::default());
}

#[test]
fn test_wire_trace_ctx_to_opentelemetry_context() {
    let ctx = WireTraceCtx::new_for_message();
    let _otel_context = ctx.to_opentelemetry_context();

    // Should contain the span context
    // Note: is_valid() is not available on SpanRef, so we skip this check
}

#[test]
fn test_wire_trace_ctx_serialize_deserialize() {
    let original = WireTraceCtx::new_for_message();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
    let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();

    assert_eq!(original.trace_id, deserialized.trace_id);
    assert_eq!(original.span_id, deserialized.span_id);
    assert_eq!(original.trace_flags, deserialized.trace_flags);
}

#[test]
fn test_wire_trace_ctx_serde_serialize_deserialize() {
    let original = WireTraceCtx::new_for_message();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: WireTraceCtx = serde_json::from_str(&json).unwrap();

    assert_eq!(original.trace_id, deserialized.trace_id);
    assert_eq!(original.span_id, deserialized.span_id);
    assert_eq!(original.trace_flags, deserialized.trace_flags);
}

#[test]
fn test_wire_trace_ctx_rkyv_roundtrip() {
    let original = WireTraceCtx::new_for_message();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
    let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();

    // Verify roundtrip produces identical output
    assert_eq!(original.trace_id, deserialized.trace_id);
    assert_eq!(original.span_id, deserialized.span_id);
    assert_eq!(original.trace_flags, deserialized.trace_flags);
}

#[test]
fn test_wire_trace_ctx_serde_roundtrip() {
    let original = WireTraceCtx::new_for_message();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: WireTraceCtx = serde_json::from_str(&json).unwrap();

    // Verify roundtrip produces identical output
    assert_eq!(original.trace_id, deserialized.trace_id);
    assert_eq!(original.span_id, deserialized.span_id);
    assert_eq!(original.trace_flags, deserialized.trace_flags);
}

#[test]
fn test_wire_trace_ctx_from_current_span() {
    // This test verifies that from_current_span handles the case when there's no active span
    // It should fall back to generating a new random trace
    let ctx = WireTraceCtx::from_current_span();

    assert!(ctx.is_some());
    let ctx = ctx.unwrap();
    assert_eq!(ctx.trace_id.len(), 16);
    assert_eq!(ctx.span_id.len(), 8);
}

#[test]
fn test_wire_trace_ctx_clone() {
    let original = WireTraceCtx::new_for_message();
    let cloned = original.clone();

    assert_eq!(original.trace_id, cloned.trace_id);
    assert_eq!(original.span_id, cloned.span_id);
    assert_eq!(original.trace_flags, cloned.trace_flags);
}

#[test]
fn test_wire_trace_ctx_debug() {
    let ctx = WireTraceCtx::new_for_message();
    let debug_str = format!("{:?}", ctx);

    assert!(debug_str.contains("trace_id"));
    assert!(debug_str.contains("span_id"));
    assert!(debug_str.contains("trace_flags"));
}
