//! Validate-and-format gate for every emitted source fragment.
//!
//! Every `EmittedItem.source` a template returns flows through this
//! module before any byte hits disk. Two stages:
//!
//! 1. **Parse** with `syn::parse_file`. If parsing fails, the template
//!    emitted broken Rust — we surface this as
//!    `CodegenError::InvalidEmission` so the operator (and the test
//!    suite) sees the bug immediately rather than writing
//!    syntactically-broken source to disk.
//! 2. **Format** with `prettyplease::unparse`. Output is deterministic by
//!    design — re-running this on the same input yields the same bytes.
//!    That property is what makes idempotent regen byte-identical.
//!
//! Note: prettyplease formats AST → string, so we pay one parse and one
//! unparse per emission. Templates that already produce well-formatted
//! source still get the determinism guarantee; templates that produce
//! sloppy source get cleaned up. Either way, the on-disk artefact is
//! always the canonical form.

use crate::codegen::CodegenError;

/// Parse + format `source`. Returns the canonical pretty-printed Rust on
/// success, or a `CodegenError::InvalidEmission` tagged with the emitting
/// template / node on parse failure.
///
/// `template_id` and `node_id` are recorded in the error so a failing
/// emission can be traced back to its source. They're free-form strings
/// at this layer; the caller (the generator) supplies them from the
/// `NodeTemplate` context.
pub fn validate_and_format(
    source: &str,
    template_id: &str,
    node_id: &str,
) -> Result<String, CodegenError> {
    let file: syn::File = syn::parse_file(source).map_err(|err| CodegenError::InvalidEmission {
        template_id: template_id.to_string(),
        node_id: node_id.to_string(),
        error: err.to_string(),
    })?;
    Ok(prettyplease::unparse(&file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_and_format_round_trips_well_formed_source() {
        let src = "fn answer() -> u32 { 42 }";
        let out = validate_and_format(src, "test.template", "n1").unwrap();
        // prettyplease canonicalises whitespace. The exact output depends
        // on the version; we just assert it's non-empty and contains the
        // function name.
        assert!(out.contains("fn answer"));
        assert!(out.contains("42"));
    }

    #[test]
    fn test_validate_and_format_is_idempotent() {
        // The determinism property is what idempotent regen relies on.
        let src = "fn a(){let x=1;let y=2;x+y}";
        let once = validate_and_format(src, "t", "n").unwrap();
        let twice = validate_and_format(&once, "t", "n").unwrap();
        assert_eq!(once, twice, "formatting must be deterministic");
    }

    #[test]
    fn test_validate_rejects_malformed_source() {
        let src = "fn broken( { not rust";
        let err = validate_and_format(src, "buggy.template", "n7").unwrap_err();
        match err {
            CodegenError::InvalidEmission {
                template_id,
                node_id,
                ..
            } => {
                assert_eq!(template_id, "buggy.template");
                assert_eq!(node_id, "n7");
            }
            other => panic!("expected InvalidEmission, got {other:?}"),
        }
    }
}
