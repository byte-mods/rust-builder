//! `@generated` region merge — splice template-emitted bodies into an
//! existing file while preserving user code outside the regions.
//!
//! ## Marker grammar
//!
//! ```text
//! // @generated:begin <id>
//! ... body ...
//! // @generated:end <id>
//! ```
//!
//! `<id>` is `<template_id>:<node_id>:<site>` by convention — stable across
//! regens, traceable back to its emitting template.
//!
//! Markers are whole-line comments. Anything before the `//` on the same
//! line (e.g. indentation whitespace) is permitted and preserved. The id
//! on the begin and end lines must match exactly.
//!
//! ## Merge rules
//!
//! - **Existing region** (id present in both target file and new
//!   emission): body inside the markers is replaced; markers themselves
//!   and the lines outside remain.
//! - **New region** (id in new emission only): appended to the end of
//!   the target file wrapped in fresh markers, with a blank-line buffer
//!   before it.
//! - **Stale region** (id in target file only): removed along with its
//!   markers, since the corresponding node no longer exists in the graph.
//! - **Lines outside any region**: preserved verbatim.
//!
//! Nested regions are **disallowed** in v1 — the parser errors out on
//! encountering a `begin` inside another open region. This keeps the
//! splice logic linear and easy to reason about; the only downside is
//! templates can't emit hierarchical generated content, which no current
//! built-in needs.

use std::collections::HashMap;

use thiserror::Error;

/// Single source of truth for the marker prefixes. Kept as constants so a
/// future syntax change is one diff.
const BEGIN_PREFIX: &str = "// @generated:begin ";
const END_PREFIX: &str = "// @generated:end ";

/// Reasons a merge can fail. The target file may be authored by a user;
/// the new emission is authored by templates. Either side can be
/// inconsistent in principle, though the template side is far more
/// likely (and surfaces as an `InvalidEmission` upstream via the format
/// pass).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegionError {
    #[error("region begin marker for `{0}` has no matching end")]
    UnclosedRegion(String),
    #[error("region end marker for `{0}` without matching begin")]
    StrayEnd(String),
    #[error("region `{begin_id}` begin/end ids mismatch (end says `{end_id}`)")]
    Mismatched { begin_id: String, end_id: String },
    #[error("region `{0}` opens before previous region closes (nested regions are not supported)")]
    Nested(String),
    #[error("region `{0}` declared twice in the same file")]
    Duplicate(String),
}

/// Merge result. `text` is the new file contents to write; the other
/// fields are diagnostics surfaced by the orchestrator for the regen
/// report.
#[derive(Debug)]
pub struct MergeOutcome {
    pub text: String,
    pub regions_updated: Vec<String>,
    pub regions_added: Vec<String>,
    pub regions_removed: Vec<String>,
}

/// Parse an existing file into (region_id → body) plus a "skeleton" of
/// non-region lines with placeholders where regions used to live. Used
/// by [`merge`] but also useful for tests.
struct Parsed<'a> {
    /// File lines split by `\n`, with original endings recovered when we
    /// re-emit.
    lines: Vec<&'a str>,
    /// For every region we found, the half-open line range
    /// `[begin_line, end_line]` (inclusive on both ends — both marker
    /// lines are part of the region for replacement purposes).
    region_spans: Vec<(String, usize, usize)>,
}

fn parse_regions(text: &str) -> Result<Parsed<'_>, RegionError> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut spans: Vec<(String, usize, usize)> = Vec::new();
    let mut open: Option<(String, usize)> = None;
    let mut seen_ids: HashMap<String, ()> = HashMap::new();

    for (i, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix(BEGIN_PREFIX) {
            let id = rest.trim().to_string();
            if let Some((open_id, _)) = open.as_ref() {
                return Err(RegionError::Nested(open_id.clone()));
            }
            if seen_ids.contains_key(&id) {
                return Err(RegionError::Duplicate(id));
            }
            seen_ids.insert(id.clone(), ());
            open = Some((id, i));
        } else if let Some(rest) = trimmed.strip_prefix(END_PREFIX) {
            let end_id = rest.trim().to_string();
            match open.take() {
                Some((begin_id, begin_line)) if begin_id == end_id => {
                    spans.push((begin_id, begin_line, i));
                }
                Some((begin_id, _)) => {
                    return Err(RegionError::Mismatched { begin_id, end_id });
                }
                None => return Err(RegionError::StrayEnd(end_id)),
            }
        }
    }

    if let Some((id, _)) = open {
        return Err(RegionError::UnclosedRegion(id));
    }

    Ok(Parsed { lines, region_spans: spans })
}

/// Merge `new_regions` into the existing file `target_text`.
///
/// `new_regions` is keyed by region id; the value is the body that
/// should appear *between* the marker lines (markers are not part of
/// the body — the merger adds them).
///
/// Returns the new file contents plus diagnostics about what changed.
pub fn merge(
    target_text: &str,
    new_regions: &HashMap<String, String>,
) -> Result<MergeOutcome, RegionError> {
    let parsed = parse_regions(target_text)?;

    // 1) Build the new text by walking the existing lines and replacing
    //    each region's body in-place when its id is in `new_regions`,
    //    or stripping the entire region (markers + body) when it is
    //    stale.
    let mut out = String::with_capacity(target_text.len());
    let mut updated: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let region_by_begin: HashMap<usize, &(String, usize, usize)> = parsed
        .region_spans
        .iter()
        .map(|s| (s.1, s))
        .collect();

    let mut i = 0usize;
    while i < parsed.lines.len() {
        if let Some(span) = region_by_begin.get(&i) {
            let (id, begin_line, end_line) = (&span.0, span.1, span.2);
            if let Some(new_body) = new_regions.get(id) {
                // Replace: re-emit begin marker (preserving original
                // line, including any indentation), then the new body,
                // then the end marker.
                out.push_str(parsed.lines[begin_line]);
                out.push('\n');
                out.push_str(new_body);
                if !new_body.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(parsed.lines[end_line]);
                if end_line + 1 < parsed.lines.len() {
                    out.push('\n');
                }
                updated.push(id.clone());
            } else {
                // Stale region: drop markers + body entirely. Squash any
                // trailing blank we may have just emitted to avoid a
                // run of empty lines.
                removed.push(id.clone());
            }
            i = end_line + 1;
            continue;
        }
        out.push_str(parsed.lines[i]);
        if i + 1 < parsed.lines.len() {
            out.push('\n');
        }
        i += 1;
    }

    // 2) Append any region in `new_regions` that wasn't in the existing
    //    file. Deterministic order — keys sorted lexicographically so
    //    idempotent regen produces idempotent output.
    let existing_ids: std::collections::HashSet<&str> =
        parsed.region_spans.iter().map(|s| s.0.as_str()).collect();
    let mut to_add: Vec<&String> = new_regions
        .keys()
        .filter(|k| !existing_ids.contains(k.as_str()))
        .collect();
    to_add.sort();

    let mut added: Vec<String> = Vec::new();
    for id in to_add {
        let body = &new_regions[id];
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(BEGIN_PREFIX);
        out.push_str(id);
        out.push('\n');
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(END_PREFIX);
        out.push_str(id);
        out.push('\n');
        added.push(id.clone());
    }

    Ok(MergeOutcome {
        text: out,
        regions_updated: updated,
        regions_added: added,
        regions_removed: removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn test_merge_into_empty_file_appends_all_regions() {
        let result = merge("", &regions(&[("a", "let a = 1;")])).unwrap();
        assert!(result.text.contains("// @generated:begin a"));
        assert!(result.text.contains("let a = 1;"));
        assert!(result.text.contains("// @generated:end a"));
        assert_eq!(result.regions_added, vec!["a"]);
        assert!(result.regions_updated.is_empty());
        assert!(result.regions_removed.is_empty());
    }

    #[test]
    fn test_merge_replaces_existing_region_body_preserves_outside() {
        let existing = "\
fn user_function() {
    // user code preserved
}

// @generated:begin route.r1
fn old_handler() {}
// @generated:end route.r1

// trailing user comment
";
        let updates = regions(&[("route.r1", "fn new_handler() {}")]);
        let result = merge(existing, &updates).unwrap();
        assert!(
            result.text.contains("fn user_function"),
            "user code outside region must survive"
        );
        assert!(
            result.text.contains("// trailing user comment"),
            "user comments outside regions must survive"
        );
        assert!(
            result.text.contains("fn new_handler"),
            "new body must replace old"
        );
        assert!(
            !result.text.contains("fn old_handler"),
            "old body must be gone"
        );
        assert_eq!(result.regions_updated, vec!["route.r1"]);
    }

    #[test]
    fn test_merge_removes_stale_regions() {
        let existing = "\
// @generated:begin gone.n1
fn dead() {}
// @generated:end gone.n1
";
        let result = merge(existing, &HashMap::new()).unwrap();
        assert!(!result.text.contains("gone.n1"));
        assert!(!result.text.contains("fn dead"));
        assert_eq!(result.regions_removed, vec!["gone.n1"]);
    }

    #[test]
    fn test_merge_unclosed_region_errors() {
        let existing = "// @generated:begin a\nfn x() {}\n";
        let err = merge(existing, &HashMap::new()).unwrap_err();
        assert_eq!(err, RegionError::UnclosedRegion("a".into()));
    }

    #[test]
    fn test_merge_stray_end_errors() {
        let existing = "// @generated:end orphan\n";
        let err = merge(existing, &HashMap::new()).unwrap_err();
        assert_eq!(err, RegionError::StrayEnd("orphan".into()));
    }

    #[test]
    fn test_merge_mismatched_ids_error() {
        let existing = "// @generated:begin a\n// @generated:end b\n";
        let err = merge(existing, &HashMap::new()).unwrap_err();
        assert_eq!(
            err,
            RegionError::Mismatched {
                begin_id: "a".into(),
                end_id: "b".into()
            }
        );
    }

    #[test]
    fn test_merge_nested_region_errors() {
        let existing = "// @generated:begin outer\n// @generated:begin inner\n// @generated:end inner\n// @generated:end outer\n";
        let err = merge(existing, &HashMap::new()).unwrap_err();
        assert_eq!(err, RegionError::Nested("outer".into()));
    }

    #[test]
    fn test_merge_duplicate_id_in_existing_file_errors() {
        let existing = "\
// @generated:begin dup
// @generated:end dup
// @generated:begin dup
// @generated:end dup
";
        let err = merge(existing, &HashMap::new()).unwrap_err();
        assert_eq!(err, RegionError::Duplicate("dup".into()));
    }

    #[test]
    fn test_merge_is_idempotent_for_replacement() {
        let existing = "// @generated:begin r\nold\n// @generated:end r\n";
        let updates = regions(&[("r", "new body")]);
        let once = merge(existing, &updates).unwrap();
        let twice = merge(&once.text, &updates).unwrap();
        assert_eq!(once.text, twice.text, "merge twice must be byte-identical");
    }

    #[test]
    fn test_merge_appends_new_regions_in_deterministic_order() {
        // Two new regions, given in non-sorted order — the output must
        // place them lexicographically for idempotence.
        let updates = regions(&[("z.last", "z"), ("a.first", "a")]);
        let r = merge("", &updates).unwrap();
        let a_pos = r.text.find("a.first").unwrap();
        let z_pos = r.text.find("z.last").unwrap();
        assert!(a_pos < z_pos, "regions must be appended in sorted id order");
    }
}
