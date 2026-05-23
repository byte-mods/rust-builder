//! Studio HTTP error surface.
//!
//! Every Axum handler in this crate returns `Result<T, ApiError>` and lets the
//! `IntoResponse` impl below map the variant to the correct HTTP status plus a
//! stable JSON body `{"error": "<code>", "message": "<human-readable>"}`.
//!
//! **Sanitisation invariant.** The `message` field shipped to the client must
//! never contain a raw `io::Error::to_string()` (which can include absolute
//! filesystem paths and leak deployment topology) or any other internal
//! detail that the client doesn't already know. Every variant uses a fixed,
//! sanitised human message; the full underlying error is logged on the
//! server via `tracing::error!` for operator diagnosis.
//!
//! Status-code policy:
//! - `NotFound`        → 404
//! - `AlreadyExists`   → 409
//! - `InvalidSlug`     → 422 (unprocessable entity — the request was
//!                              well-formed but semantically rejected)
//! - `InvalidGraph`    → 422
//! - `Io`              → 500
//! - `Internal`        → 500

use axum::{
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::io;
use thiserror::Error;
use tracing::error;

/// Single error type returned by every studio handler.
///
/// Variants carry the minimum context needed for the server to log a useful
/// diagnostic; the JSON shipped to the client is sanitised in `IntoResponse`.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Requested resource does not exist (e.g. unknown project slug).
    #[error("not found")]
    NotFound,

    /// Resource conflict — e.g. trying to create a project whose slug is taken.
    #[error("already exists")]
    AlreadyExists,

    /// The slug field failed validation. Carries the validation reason for
    /// the server log; the client sees a generic human message.
    #[error("invalid slug: {0}")]
    InvalidSlug(String),

    /// The graph body failed validation (malformed JSON, schema mismatch,
    /// dangling edge, etc.). Carries the reason for the server log.
    #[error("invalid graph: {0}")]
    InvalidGraph(String),

    /// The request body could not be deserialised into the expected shape
    /// (malformed JSON, wrong type, validation rejection from a typed field
    /// like `Slug`). The carried string is logged server-side; the client
    /// sees a single sanitised message regardless of the underlying cause,
    /// since the raw error text may include serde column/line numbers or
    /// `Slug` validator details that should not leak.
    #[error("invalid body: {0}")]
    InvalidBody(String),

    /// Underlying I/O failure. `From<io::Error>` keeps `?` ergonomic in the
    /// store; the variant's `Display` is logged server-side but never shipped.
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// A build is already running for the requested project.
    #[error("build in progress")]
    BuildInProgress,

    /// A run is already active for the requested project.
    #[error("run in progress")]
    RunInProgress,

    /// No run is active for the requested project.
    #[error("not running")]
    NotRunning,

    /// Catch-all for unexpected conditions we don't want the client to
    /// distinguish (typically a bug or transient infrastructure failure).
    #[error("internal: {0}")]
    Internal(String),

    /// ANTHROPIC_API_KEY environment variable is not set.
    #[error("ANTHROPIC_API_KEY environment variable is not set")]
    ApiKeyMissing,

    /// Anthropic API error.
    #[error("Anthropic API error: {0}")]
    LlmError(String),
}

/// Wire shape for the JSON body returned on every error response.
///
/// Fields are stable across versions — the frontend's `ApiError` type
/// matches this shape exactly (see `frontend/src/api.ts`).
#[derive(Serialize)]
struct ApiErrorBody {
    /// Stable snake_case error code. Frontend can switch on this.
    error: &'static str,
    /// Sanitised human-readable message. Safe to surface in UI directly.
    message: &'static str,
}

impl ApiError {
    /// Map a variant to `(http_status, error_code, sanitised_message)`.
    ///
    /// Returning a 3-tuple (rather than building the response inline) makes
    /// the policy easy to audit and lets unit tests pin every mapping.
    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found", "the requested resource does not exist"),
            ApiError::AlreadyExists => (
                StatusCode::CONFLICT,
                "already_exists",
                "a resource with that identifier already exists",
            ),
            ApiError::InvalidSlug(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_slug",
                "slug must be 2-40 characters, lowercase, [a-z0-9-], not starting or ending with a hyphen",
            ),
            ApiError::InvalidGraph(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_graph",
                "the supplied graph body is malformed or violates the schema",
            ),
            ApiError::InvalidBody(_) => (
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "the request body is malformed or fails field validation",
            ),
            ApiError::Io(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "io",
                "an internal i/o error occurred",
            ),
            ApiError::BuildInProgress => (
                StatusCode::CONFLICT,
                "build_in_progress",
                "a build is already running for this project",
            ),
            ApiError::RunInProgress => (
                StatusCode::CONFLICT,
                "run_in_progress",
                "a run is already active for this project",
            ),
            ApiError::NotRunning => (
                StatusCode::CONFLICT,
                "not_running",
                "no active run for this project",
            ),
            ApiError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "an internal error occurred",
            ),
            ApiError::ApiKeyMissing => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_key_missing",
                "ANTHROPIC_API_KEY environment variable is not set",
            ),
            ApiError::LlmError(_) => (
                StatusCode::BAD_REQUEST,
                "llm_error",
                "an error occurred while generating the flow graph",
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message) = self.parts();
        // Log the full Debug rep server-side so operators see the underlying
        // cause; this is the ONLY place that captures the raw detail.
        error!(
            error_code = %error,
            status = %status.as_u16(),
            detail = ?self,
            "responding with ApiError"
        );
        (status, Json(ApiErrorBody { error, message })).into_response()
    }
}

/// Convert `serde_json::Error` directly into `InvalidGraph`. Used by the
/// graph PUT handler so deserialisation failures surface as 422 with the
/// `invalid_graph` code rather than a generic 500.
impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        ApiError::InvalidGraph(err.to_string())
    }
}

/// Convert `RunError` into the appropriate `ApiError` variant.
impl From<crate::run::RunError> for ApiError {
    fn from(err: crate::run::RunError) -> Self {
        match err {
            crate::run::RunError::AlreadyRunning(_) => ApiError::RunInProgress,
            crate::run::RunError::NotRunning(_) => ApiError::NotRunning,
            crate::run::RunError::Io(io) => io.into(),
        }
    }
}

/// Map Axum's body-extraction rejection into `InvalidBody` so client-facing
/// JSON parse failures (including typed-field validation like `Slug`)
/// return a 400 with the sanitised `invalid_body` envelope rather than
/// Axum's default plain-text 400 that leaks serde detail and column numbers.
impl From<JsonRejection> for ApiError {
    fn from(rej: JsonRejection) -> Self {
        ApiError::InvalidBody(rej.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant must map to a status code in the documented set
    /// (404 / 409 / 422 / 500) and never expose internal detail in the
    /// sanitised message.
    #[test]
    fn test_api_error_status_mapping_is_documented() {
        let cases = [
            (ApiError::NotFound, StatusCode::NOT_FOUND, "not_found"),
            (ApiError::AlreadyExists, StatusCode::CONFLICT, "already_exists"),
            (
                ApiError::InvalidSlug("contains /".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_slug",
            ),
            (
                ApiError::InvalidGraph("missing nodes".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_graph",
            ),
            (
                ApiError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "/etc/secret")),
                StatusCode::INTERNAL_SERVER_ERROR,
                "io",
            ),
            (
                ApiError::BuildInProgress,
                StatusCode::CONFLICT,
                "build_in_progress",
            ),
            (
                ApiError::RunInProgress,
                StatusCode::CONFLICT,
                "run_in_progress",
            ),
            (
                ApiError::NotRunning,
                StatusCode::CONFLICT,
                "not_running",
            ),
            (
                ApiError::Internal("panic-recovered".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
        ];

        for (err, expected_status, expected_code) in cases {
            let (status, code, message) = err.parts();
            assert_eq!(status, expected_status, "status for {code}");
            assert_eq!(code, expected_code);
            // Sanitisation: the sensitive path "/etc/secret" must not appear
            // in the client-facing message under any variant.
            assert!(
                !message.contains("/etc/secret"),
                "message must not leak io::Error detail"
            );
        }
    }

    #[test]
    fn test_serde_json_error_maps_to_invalid_graph() {
        let json_err = serde_json::from_str::<u32>("not a number").unwrap_err();
        let api_err: ApiError = json_err.into();
        let (status, code, _) = api_err.parts();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(code, "invalid_graph");
    }
}
