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

use opentelemetry::Context;
use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
use rand::prelude::*;
use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

#[derive(Debug, Clone, Copy, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
#[repr(C)]
pub struct WireTraceCtx {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub trace_flags: u8,
    #[serde(skip)]
    _reserved: [u8; 7], // Pad to 32 bytes for alignment
}

impl WireTraceCtx {
    // Add a method to generate a new trace for a message
    pub fn new_for_message() -> Self {
        let mut rng = rand::rng();
        let trace_id: [u8; 16] = rng.random();
        let span_id: [u8; 8] = rng.random();

        Self {
            trace_id,
            span_id,
            trace_flags: 1,
            _reserved: [0; 7],
        }
    }

    // Helper to create child spans
    pub fn create_child(&self) -> Self {
        let mut child = *self;
        let mut rng = rand::rng();
        child.span_id = rng.random();
        child
    }

    pub fn from_current_span() -> Option<Self> {
        let span = Span::current();
        let otel_context = span.context();

        // Get the span reference
        let span_ref = otel_context.span();
        let span_context = span_ref.span_context();

        // Check if valid before creating WireTraceCtx
        if !span_context.is_valid() {
            // No active span – generate a new random trace ID for this message.
            // This prevents panics when tracing is disabled (no subscriber).
            return Some(Self::new_for_message());
        }
        Some(Self {
            trace_id: span_context.trace_id().to_bytes(),
            span_id: span_context.span_id().to_bytes(),
            trace_flags: span_context.trace_flags().to_u8(),
            _reserved: [0; 7],
        })
    }

    pub fn from_span_context(span_context: &SpanContext) -> Option<Self> {
        // Check validity first
        if !span_context.is_valid() {
            // fallback: create a new random trace
            return Some(Self::new_for_message());
        }

        Some(Self {
            trace_id: span_context.trace_id().to_bytes(),
            span_id: span_context.span_id().to_bytes(),
            trace_flags: span_context.trace_flags().to_u8(),
            _reserved: [0; 7],
        })
    }

    pub fn to_span_context(&self) -> SpanContext {
        SpanContext::new(
            TraceId::from_bytes(self.trace_id),
            SpanId::from_bytes(self.span_id),
            TraceFlags::new(self.trace_flags),
            true, // This is a remote span context
            TraceState::default(),
        )
    }

    pub fn to_opentelemetry_context(&self) -> Context {
        let span_context = self.to_span_context();
        // Create a context with this span as the remote parent
        Context::current().with_remote_span_context(span_context)
    }
}
