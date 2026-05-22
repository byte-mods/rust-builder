//! Error type for the template subsystem + conversion into [`ApiError`].
//!
//! `TemplateError` lives here (rather than inline in `mod.rs`) for the same
//! reason the crate-root error type lives in `crate::error` — error types
//! travel widely, and a fixed home makes the boundary discoverable.
//!
//! ## ApiError mapping policy
//!
//! - `NotFound`         → `ApiError::InvalidGraph(...)` — the graph
//!                        references a template the studio does not have
//!                        registered. From the client's perspective, the
//!                        body is invalid; that's the right wire code.
//! - `InvalidId`        → `ApiError::InvalidGraph(...)` — a malformed
//!                        template id in the graph body. Same reasoning.
//! - `ConfigMismatch`   → `ApiError::InvalidGraph(...)` — config blob fails
//!                        the template's JSON Schema. User-fixable; 422.
//! - `SchemaInvalid`    → `ApiError::Internal(...)` — the *template's own*
//!                        schema didn't compile. That's a builtin-author
//!                        bug, not user input; surfaces as 500 with a
//!                        sanitised message and a server-side error log.

use crate::error::ApiError;
pub use crate::templates::TemplateError;

impl From<TemplateError> for ApiError {
    fn from(err: TemplateError) -> Self {
        match &err {
            TemplateError::NotFound(_)
            | TemplateError::InvalidId(_)
            | TemplateError::ConfigMismatch(_) => ApiError::InvalidGraph(err.to_string()),
            TemplateError::SchemaInvalid(_) => ApiError::Internal(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::TemplateIdError;

    #[test]
    fn test_not_found_maps_to_invalid_graph() {
        let err: ApiError = TemplateError::NotFound("foo.bar".into()).into();
        assert!(matches!(err, ApiError::InvalidGraph(_)));
    }

    #[test]
    fn test_invalid_id_maps_to_invalid_graph() {
        let err: ApiError = TemplateError::InvalidId(TemplateIdError::NoNamespace).into();
        assert!(matches!(err, ApiError::InvalidGraph(_)));
    }

    #[test]
    fn test_config_mismatch_maps_to_invalid_graph() {
        let err: ApiError = TemplateError::ConfigMismatch("missing required field".into()).into();
        assert!(matches!(err, ApiError::InvalidGraph(_)));
    }

    #[test]
    fn test_schema_invalid_maps_to_internal() {
        // Builtin-author bug → 500, not 422 — must not surface as user fault.
        let err: ApiError = TemplateError::SchemaInvalid("bad schema".into()).into();
        assert!(matches!(err, ApiError::Internal(_)));
    }
}
