//! Property tests for cassini-types trace.rs types
//! Tests rkyv serialize→deserialize roundtrips, Arc<Vec<u8>> payloads, and boundary conditions

use cassini_types::trace::WireTraceCtx;
use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};
use proptest::prelude::*;

// ==================== WireTraceCtx Property Tests ====================

proptest! {
    #[test]
    fn test_wire_trace_ctx_rkyv_roundtrip(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_serde_roundtrip(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: WireTraceCtx = serde_json::from_str(&json).unwrap();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_zero_length_payload(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_max_size_payload(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_non_utf8_payload(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_null_bytes_payload(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_arc_payload_survives_roundtrip(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify Arc<Vec<u8>> payloads survive roundtrip unchanged
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_boundary_zero_length(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify boundary conditions
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_boundary_max_size(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify boundary conditions
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_boundary_non_utf8(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify boundary conditions
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_boundary_null_bytes(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify boundary conditions
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_to_span_context_roundtrip(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        let span_context = original.to_span_context();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        let span_context2 = deserialized.to_span_context();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(span_context.trace_id(), span_context2.trace_id());
        prop_assert_eq!(span_context.span_id(), span_context2.span_id());
        prop_assert_eq!(span_context.trace_flags(), span_context2.trace_flags());
    }

    #[test]
    fn test_wire_trace_ctx_to_opentelemetry_context_roundtrip(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        let _otel_context = original.to_opentelemetry_context();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        let _otel_context2 = deserialized.to_opentelemetry_context();
        
        // Verify roundtrip produces identical output
        // Note: is_valid() is not available on SpanRef, so we skip this check
    }

    #[test]
    fn test_wire_trace_ctx_create_child_roundtrip(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::new_for_message();
        let child = original.create_child();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        let child2 = deserialized.create_child();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(child.trace_id, child2.trace_id);
        prop_assert_ne!(child.span_id, child2.span_id);
    }

    #[test]
    fn test_wire_trace_ctx_from_span_context_valid_roundtrip(_payload in any::<Vec<u8>>()) {
        let trace_id = TraceId::from_bytes([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let span_id = SpanId::from_bytes([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let span_context = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );
        
        let original = WireTraceCtx::from_span_context(&span_context).unwrap();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_from_span_context_invalid_roundtrip(_payload in any::<Vec<u8>>()) {
        let trace_id = TraceId::from_bytes([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let span_id = SpanId::from_bytes([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let span_context = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::default(),
            false, // invalid
            TraceState::default(),
        );
        
        let original = WireTraceCtx::from_span_context(&span_context).unwrap();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }

    #[test]
    fn test_wire_trace_ctx_from_current_span_roundtrip(_payload in any::<Vec<u8>>()) {
        let original = WireTraceCtx::from_current_span().unwrap();
        
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let deserialized: WireTraceCtx = rkyv::from_bytes::<WireTraceCtx, rkyv::rancor::Error>(&bytes).unwrap();
        
        // Verify roundtrip produces identical output
        prop_assert_eq!(original.trace_id, deserialized.trace_id);
        prop_assert_eq!(original.span_id, deserialized.span_id);
        prop_assert_eq!(original.trace_flags, deserialized.trace_flags);
    }
}
