//! Axum HTTP server module.
//!
//! Wraps the `Handler` in HTTP transport. The handler itself is
//! transport-agnostic; this module is the only place that knows
//! about HTTP request/response shapes, status codes from the
//! perspective of a hyper response, and header parsing.
//!
//! # Why this is a separate module
//!
//! The integration tests exercise `Handler::handle` directly with
//! synthetic tokens and CSRs, bypassing HTTP entirely. That's the
//! right level of test for the handler logic — it's faster, more
//! deterministic, and produces clearer failure messages than
//! sending requests through a real HTTP stack. This module is
//! tested by the binary smoke test (in CI), not by the unit tests.
//!
//! The split also means the handler can be embedded in a different
//! transport later — say, a gRPC interface or an in-process call
//! from a colocated orchestrator — without rewriting the
//! validation logic.

use crate::handler::{Handler, HandlerBody, HandlerResponse};
use axum::Router;
use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use cert_issuer_common::{IssueError, IssueOutcome, IssueRequest};
use std::sync::Arc;
use tracing::error;

/// Build the Axum router for the cert issuer.
///
/// The `Handler` is wrapped in an `Arc` for cheap cloning across
/// request handlers — Axum's state extractor requires `Clone`, and
/// `Arc<Handler>` satisfies that without forcing `Handler` itself
/// to be `Clone`.
pub fn build_router(handler: Arc<Handler>) -> Router {
    Router::new()
        .route("/issue", post(issue))
        .with_state(handler)
}

/// Handler for `POST /issue`.
///
/// Extracts the bearer token from the `Authorization` header,
/// extracts the request body as JSON, and forwards both to the
/// `Handler::handle` method. The result is converted back to an
/// HTTP response.
///
/// **Header parsing failure modes:**
///
/// - Missing `Authorization` header → 401 with
///   `IssueOutcome::InvalidToken` and a clear "missing header"
///   detail. Operators chasing this error in the audit log see
///   "missing Authorization header" rather than "invalid token,"
///   which is more diagnostic.
/// - Malformed `Authorization` header (not a `Bearer` scheme, or
///   empty after the scheme) → same disposition as missing.
/// - Body JSON parse failure is handled by Axum's `Json` extractor
///   automatically; it returns 400 before our handler runs.
async fn issue(
    State(handler): State<Arc<Handler>>,
    headers: HeaderMap,
    Json(request): Json<IssueRequest>,
) -> Response {
    let bearer = match extract_bearer_token(&headers) {
        Ok(t) => t,
        Err(detail) => {
            return reject(IssueOutcome::InvalidToken, detail);
        }
    };

    let result = handler.handle(&bearer, request).await;
    handler_response_to_axum(result)
}

/// Extract the bearer token from the `Authorization: Bearer <token>`
/// header.
///
/// Returns the raw token string on success. The token is not
/// inspected here — the caller passes it to OIDC validation, which
/// is the only place that should make decisions about its contents.
fn extract_bearer_token(headers: &HeaderMap) -> Result<String, &'static str> {
    let header_value = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or("missing Authorization header")?;

    let header_str = header_value
        .to_str()
        .map_err(|_| "Authorization header is not valid UTF-8")?;

    // Case-sensitive on "Bearer" — the HTTP spec is case-insensitive
    // here in theory but the only legitimate producer in our
    // environment is the init container, which always emits "Bearer ".
    // Being strict catches misconfigurations rather than silently
    // accepting them.
    let token = header_str
        .strip_prefix("Bearer ")
        .ok_or("Authorization header is not a Bearer credential")?
        .trim();

    if token.is_empty() {
        return Err("Bearer credential is empty");
    }

    Ok(token.to_string())
}

/// Convert a `HandlerResponse` to an Axum response.
///
/// Status code from the handler maps directly to the HTTP status
/// code. Body is serialized as JSON in either the success or error
/// shape, matching what the init container expects to receive.
fn handler_response_to_axum(resp: HandlerResponse) -> Response {
    let status = StatusCode::from_u16(resp.status).unwrap_or_else(|_| {
        // Defensive: handler returned a status outside the valid
        // u16 range. Should be impossible given `status_for_outcome`
        // returns hard-coded values, but if we ever drift, log and
        // return 500.
        error!(status = resp.status, "handler produced invalid status");
        StatusCode::INTERNAL_SERVER_ERROR
    });

    match resp.body {
        HandlerBody::Success(success) => (status, Json(success)).into_response(),
        HandlerBody::Error(error) => (status, Json(error)).into_response(),
    }
}

/// Build a rejection response for header-extraction failures.
///
/// These don't go through the handler (we never made it that far),
/// so we emit the response directly with an appropriate outcome and
/// detail.
fn reject(outcome: IssueOutcome, detail: &'static str) -> Response {
    let status = StatusCode::from_u16(crate::handler::status_for_outcome(outcome))
        .unwrap_or(StatusCode::UNAUTHORIZED);
    let body = IssueError {
        outcome,
        detail: detail.to_string(),
    };
    (status, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// Tests for the HTTP layer specifically
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! Tests for the Axum layer's responsibilities: header
    //! extraction, JSON serialization, status code mapping.
    //!
    //! These are NOT end-to-end tests — they don't exercise OIDC
    //! validation or CA issuance. The handler's behavior is tested
    //! in `tests/handler_integration_test.rs` against a real
    //! `Handler` instance. Here we test only what the Axum layer
    //! adds on top.

    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn extracts_bearer_token_from_well_formed_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer my-token-value"),
        );
        let token = extract_bearer_token(&headers).expect("extract");
        assert_eq!(token, "my-token-value");
    }

    #[test]
    fn missing_authorization_header_is_rejected() {
        let headers = HeaderMap::new();
        let err = extract_bearer_token(&headers).expect_err("must reject");
        assert!(err.contains("missing"));
    }

    #[test]
    fn non_bearer_scheme_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        let err = extract_bearer_token(&headers).expect_err("must reject");
        assert!(err.contains("Bearer"));
    }

    #[test]
    fn empty_bearer_credential_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer "),
        );
        let err = extract_bearer_token(&headers).expect_err("must reject");
        assert!(err.contains("empty"));
    }

    #[test]
    fn case_sensitive_bearer_prefix() {
        // "bearer" lowercase doesn't match "Bearer" prefix. The HTTP
        // spec is case-insensitive on the scheme name in theory, but
        // our environment's init container always sends "Bearer " and
        // anything else is more likely a misconfiguration than
        // legitimate variation.
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("bearer my-token"),
        );
        let result = extract_bearer_token(&headers);
        assert!(result.is_err());
    }
}
