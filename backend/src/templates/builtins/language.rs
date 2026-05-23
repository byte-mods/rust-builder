//! Visual-Rust language nodes (S15a foundation).
//!
//! This module hosts node templates that let a user place Rust **language
//! constructs** on the canvas — Struct (T1), Enum (T2), Function (T3) — and
//! have the studio emit clean, formatted, compileable Rust source into the
//! user-project without any syntax being typed by hand at the type-level
//! shell.
//!
//! ## Why a dedicated module
//!
//! The single-file `builtins/mod.rs` convention applies "until a template
//! grows non-trivial codegen". Language templates emit AST nodes via
//! `syn`/`quote` and route the output through `prettyplease`, which is
//! substantively more code than the existing per-template ~30 lines. Two
//! moves are easier than ten file-splits later, so the new namespace
//! gets its own file from day one.
//!
//! ## Codegen invariants
//!
//! - Names are **CamelCase Rust identifiers**, validated at schema time via
//!   [`is_valid_camel_case_ident`]. The file path on disk uses
//!   [`to_snake_case`].
//! - Field types are **parseable `syn::Type`**, validated at schema time so
//!   garbage strings (`"Vec<{"`) fail fast with a field-indexed error
//!   rather than later via `cargo check`.
//! - Output is always run through `prettyplease::unparse` so formatting is
//!   byte-stable across calls — same input → byte-identical output is a
//!   hard invariant of the studio's idempotent-regen contract.
//! - `Serialize`/`Deserialize` derives auto-add a `serde` dependency hint
//!   to the emission so `Cargo.toml` is kept in sync without the user
//!   manually adding the crate.

use proc_macro2::TokenStream;
use quote::{format_ident, quote, ToTokens};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use syn::parse_str;

use crate::templates::{
    codegen::{CodegenCtx, EmittedItem, RuntimeEmission, SchemaEmission},
    ports::PortSpec,
    CodegenMode, DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateError, TemplateId,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// CamelCase identifier check used by every language template that accepts
/// a user-supplied type name (struct, enum, future trait/impl).
///
/// Rules: ≥1 char, first char `[A-Z]`, every subsequent char `[A-Za-z0-9]`.
/// Underscores are deliberately excluded — Rust *allows* them in type
/// names, but the studio convention (mirrored in the existing `core.dto`
/// template and the README) is strict CamelCase to keep generated source
/// readable and avoid `Foo_bar` mixups.
pub(super) fn is_valid_camel_case_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric())
}

/// snake_case identifier check used by field names and (future) function
/// parameter names. Rules: ≥1 char, first char `[a-z_]`, every subsequent
/// char `[a-z0-9_]`. Reserved keywords are NOT rejected here — `syn` will
/// fail at emission time which gives a more useful error.
pub(super) fn is_valid_snake_case_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// CamelCase → snake_case for file paths. Mirrors the helper already used
/// by the legacy templates in `builtins/mod.rs`. Duplicated here to keep
/// `language.rs` self-contained — when language templates are stable we
/// can consolidate.
pub(super) fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

pub(super) fn validate_generics_and_where(
    generics: &[GenericParam],
    where_clause: Option<&str>,
    template_name: &str,
) -> Result<(), TemplateError> {
    let mut seen_generics: HashSet<&str> = HashSet::with_capacity(generics.len());
    for (i, g) in generics.iter().enumerate() {
        if g.name.starts_with('\'') {
            if parse_str::<syn::GenericParam>(&g.name).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "{template_name}: generic param {i} name {:?} is not a valid lifetime identifier",
                    g.name
                )));
            }
            if let Some(b) = &g.bound {
                if b.trim().is_empty() {
                    return Err(TemplateError::ConfigMismatch(format!(
                        "{template_name}: generic param {i} ({:?}) has empty bound; omit the field instead",
                        g.name
                    )));
                }
                let assembled = format!("{}: {}", g.name, b);
                if parse_str::<syn::LifetimeParam>(&assembled).is_err() {
                    return Err(TemplateError::ConfigMismatch(format!(
                        "{template_name}: generic param {i} ({:?}) has invalid bound {:?}",
                        g.name, b
                    )));
                }
            }
        } else {
            if !is_valid_camel_case_ident(&g.name) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "{template_name}: generic param {i} name {:?} is not a CamelCase Rust identifier",
                    g.name
                )));
            }
            if parse_str::<syn::Ident>(&g.name).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "{template_name}: generic param {i} name {:?} is a Rust reserved word",
                    g.name
                )));
            }
            if let Some(b) = &g.bound {
                if b.trim().is_empty() {
                    return Err(TemplateError::ConfigMismatch(format!(
                        "{template_name}: generic param {i} ({:?}) has empty bound; omit the field instead",
                        g.name
                    )));
                }
                let assembled = format!("{}: {}", g.name, b);
                if parse_str::<syn::TypeParam>(&assembled).is_err() {
                    return Err(TemplateError::ConfigMismatch(format!(
                        "{template_name}: generic param {i} ({:?}) has invalid bound {:?}",
                        g.name, b
                    )));
                }
            }
        }

        if !seen_generics.insert(g.name.as_str()) {
            return Err(TemplateError::ConfigMismatch(format!(
                "{template_name}: duplicate generic param name {:?}",
                g.name
            )));
        }
    }

    if let Some(wc) = where_clause {
        let wc_str = wc.trim();
        if !wc_str.is_empty() {
            let assembled = format!("where {}", wc_str);
            if parse_str::<syn::WhereClause>(&assembled).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "{template_name}: invalid where clause syntax: {:?}",
                    wc
                )));
            }
        }
    }

    Ok(())
}

pub(super) fn format_generics(
    generics: &[GenericParam],
    where_clause: Option<&str>,
) -> Result<(TokenStream, TokenStream), TemplateError> {
    if generics.is_empty() {
        let where_tokens = if let Some(wc) = where_clause {
            let wc_str = wc.trim();
            if wc_str.is_empty() {
                quote! {}
            } else {
                let parsed_wc: syn::WhereClause = parse_str(&format!("where {}", wc_str))
                    .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid where clause: {e}")))?;
                parsed_wc.into_token_stream()
            }
        } else {
            quote! {}
        };
        return Ok((quote! {}, where_tokens));
    }

    let mut lifetimes = Vec::new();
    let mut types = Vec::new();

    for g in generics {
        if g.name.starts_with('\'') {
            let parsed: syn::GenericParam = parse_str(&g.name)
                .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid lifetime parameter name {:?}: {}", g.name, e)))?;
            let item = match &g.bound {
                Some(b) => {
                    let assembled = format!("{}: {}", g.name, b);
                    let lt: syn::LifetimeParam = parse_str(&assembled)
                        .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid lifetime bound syntax {:?}: {}", assembled, e)))?;
                    lt.into_token_stream()
                }
                None => parsed.into_token_stream(),
            };
            lifetimes.push(item);
        } else {
            if !is_valid_camel_case_ident(&g.name) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "generic type parameter name {:?} is not CamelCase",
                    g.name
                )));
            }
            if parse_str::<syn::Ident>(&g.name).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "generic type parameter name {:?} is a Rust reserved keyword",
                    g.name
                )));
            }
            let item = match &g.bound {
                Some(b) => {
                    let assembled = format!("{}: {}", g.name, b);
                    let tp: syn::TypeParam = parse_str(&assembled)
                        .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid type bound syntax {:?}: {}", assembled, e)))?;
                    tp.into_token_stream()
                }
                None => {
                    let ident = format_ident!("{}", g.name);
                    quote! { #ident }
                }
            };
            types.push(item);
        }
    }

    let mut all_params = Vec::new();
    all_params.extend(lifetimes);
    all_params.extend(types);

    let generics_tokens = quote! { < #(#all_params),* > };

    let where_tokens = if let Some(wc) = where_clause {
        let wc_str = wc.trim();
        if wc_str.is_empty() {
            quote! {}
        } else {
            let parsed_wc: syn::WhereClause = parse_str(&format!("where {}", wc_str))
                .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid where clause: {e}")))?;
            parsed_wc.into_token_stream()
        }
    } else {
        quote! {}
    };

    Ok((generics_tokens, where_tokens))
}

/// Subset of derives the UI offers in the checklist. Anything more exotic
/// can be added later; this list covers ~95% of real-world use and avoids
/// the user pasting in arbitrary trait paths (which would defeat the
/// strict-typing point of the schema).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, JsonSchema, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) enum DeriveKind {
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    Serialize,
    Deserialize,
}

impl DeriveKind {
    /// Path written into the `#[derive(...)]` attribute.
    fn as_path(self) -> &'static str {
        match self {
            DeriveKind::Debug => "Debug",
            DeriveKind::Clone => "Clone",
            DeriveKind::Copy => "Copy",
            DeriveKind::PartialEq => "PartialEq",
            DeriveKind::Eq => "Eq",
            DeriveKind::Hash => "Hash",
            DeriveKind::Default => "Default",
            DeriveKind::Serialize => "serde::Serialize",
            DeriveKind::Deserialize => "serde::Deserialize",
        }
    }

    /// Whether choosing this derive requires `serde` in `Cargo.toml`. The
    /// emission's `dependencies` field carries the hint up to the
    /// orchestrator which dedupes into one `[dependencies]` entry.
    fn needs_serde(self) -> bool {
        matches!(self, DeriveKind::Serialize | DeriveKind::Deserialize)
    }
}

/// Visibility selector mirrored across struct/enum/fn templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(super) enum Visibility {
    #[default]
    Pub,
    PubCrate,
    Private,
}

impl Visibility {
    /// Tokens to prepend before the keyword (`struct`/`enum`/`fn`). Empty
    /// string for `Private` because the absence of `pub` is the form.
    pub(super) fn as_tokens(self) -> TokenStream {
        match self {
            Visibility::Pub => quote! { pub },
            Visibility::PubCrate => quote! { pub(crate) },
            Visibility::Private => quote! {},
        }
    }
}

/// Render a `#[derive(...)]` attribute from the configured set. Order is
/// stabilised (sorted by the enum's discriminant order via the input slice
/// being passed in iteration order from the schema) so output is byte-stable.
///
/// Returns `None` when the set is empty — the caller should omit the
/// attribute entirely rather than emitting `#[derive()]`.
fn derive_attr(derives: &[DeriveKind]) -> Option<TokenStream> {
    if derives.is_empty() {
        return None;
    }
    let paths: Vec<TokenStream> = derives
        .iter()
        .map(|d| {
            // `parse_str` accepts a full path like `serde::Serialize` and
            // returns a `Path` token-stream — preferable to `format_ident`
            // here because the latter can't represent multi-segment paths.
            let path: syn::Path = parse_str(d.as_path()).expect("derive paths are static and valid");
            quote! { #path }
        })
        .collect();
    Some(quote! { #[derive(#(#paths),*)] })
}

/// Format an arbitrary `TokenStream` through `prettyplease`. Failure is a
/// builtin-author bug (we generated unparseable Rust) so it surfaces as
/// `TemplateError::SchemaInvalid` — the same channel `core.dto` uses.
pub(super) fn format_tokens(template_id: &str, tokens: TokenStream) -> Result<String, TemplateError> {
    let parsed: syn::File = syn::parse2(tokens).map_err(|e| {
        TemplateError::SchemaInvalid(format!(
            "{template_id}: emitted tokens did not parse as a syn::File: {e}"
        ))
    })?;
    Ok(prettyplease::unparse(&parsed))
}

// ---------------------------------------------------------------------------
// language.struct
// ---------------------------------------------------------------------------

/// Per-instance config for [`LanguageStruct`].
///
/// Validated by [`LanguageStructConfig::validate`] beyond what JSON Schema
/// can express (CamelCase name, parseable types, no duplicate field names).
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguageStructConfig {
    /// CamelCase Rust identifier — e.g. `User`, `OrderLine`.
    pub(super) name: String,
    /// Visibility prefix on the emitted struct.
    #[serde(default)]
    pub(super) visibility: Visibility,
    /// Derives written into a single `#[derive(...)]` line.
    #[serde(default)]
    pub(super) derives: Vec<DeriveKind>,
    /// Ordered field list. May be empty — emits a unit-style `pub struct X;`.
    #[serde(default)]
    pub(super) fields: Vec<StructField>,
    /// Generic parameters (lifetimes, then types)
    #[serde(default)]
    pub(super) generics: Vec<GenericParam>,
    /// Optional where clause for generic constraints.
    #[serde(default)]
    pub(super) where_clause: Option<String>,
}

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct StructField {
    /// snake_case field identifier.
    pub(super) name: String,
    /// Rust type literal — anything `syn::parse_str::<syn::Type>` accepts.
    /// Examples: `u64`, `String`, `Vec<u8>`, `Option<crate::dto::User>`.
    pub(super) ty: String,
    /// Per-field visibility. Defaults to `pub` to match the existing
    /// `core.dto` convention.
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl LanguageStructConfig {
    /// Semantic validation beyond JSON Schema's structural check.
    ///
    /// Errors are returned as `TemplateError::ConfigMismatch` so they
    /// surface through the same channel as schema failures and the
    /// frontend's existing config-drawer error path handles them.
    ///
    /// **Critical invariant:** every name that reaches `emit_schema` must
    /// be parseable as a plain `syn::Ident`. `format_ident!` panics on
    /// Rust reserved keywords (`Self`, `Super`, `type`, `match`, ...), so
    /// passing a keyword through validation would crash a request
    /// handler — a violation of CLAUDE.md's no-panic rule. We therefore
    /// run `syn::parse_str::<syn::Ident>` here as the load-bearing check,
    /// independent of the cosmetic CamelCase/snake_case rules.
    fn validate(&self) -> Result<(), TemplateError> {
        if !is_valid_camel_case_ident(&self.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.struct: name {:?} is not a CamelCase Rust identifier",
                self.name
            )));
        }
        // Hard syntactic check — rejects Rust reserved keywords that
        // CamelCase regex alone cannot exclude (e.g. `Self`, `Super`).
        if parse_str::<syn::Ident>(&self.name).is_err() {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.struct: name {:?} is a Rust reserved word or otherwise not a valid identifier",
                self.name
            )));
        }

        // Reject duplicate derives — emitting `#[derive(Copy, Copy)]` is
        // a guaranteed cargo-check failure that the user can fix here
        // with a clearer message than the compiler's.
        let mut seen_derives: HashSet<DeriveKind> = HashSet::with_capacity(self.derives.len());
        for d in &self.derives {
            if !seen_derives.insert(*d) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.struct: duplicate derive {:?}",
                    d
                )));
            }
        }

        let mut seen: HashSet<&str> = HashSet::with_capacity(self.fields.len());
        for (i, field) in self.fields.iter().enumerate() {
            if !is_valid_snake_case_ident(&field.name) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.struct: field {i} name {:?} is not a snake_case Rust identifier",
                    field.name
                )));
            }
            // Hard syntactic check — same reason as the struct name above.
            // `type`, `fn`, `match`, etc. pass the snake_case regex but
            // would panic in `format_ident!` at emit time.
            if parse_str::<syn::Ident>(&field.name).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.struct: field {i} name {:?} is a Rust reserved word or otherwise not a valid identifier",
                    field.name
                )));
            }
            if !seen.insert(field.name.as_str()) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.struct: duplicate field name {:?}",
                    field.name
                )));
            }
            // Reject unparseable type literals early. The `syn::Type` parser
            // catches malformed generics, missing closers, stray punctuation.
            if parse_str::<syn::Type>(&field.ty).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.struct: field {i} ({:?}) has invalid type {:?}",
                    field.name, field.ty
                )));
            }
        }

        validate_generics_and_where(&self.generics, self.where_clause.as_deref(), "language.struct")?;

        Ok(())
    }
}

/// `language.struct` — emits `[visibility] struct Name { ... }` (or the
/// unit form `[visibility] struct Name;` when no fields are configured),
/// with the chosen derive set rendered as a single `#[derive(...)]` line.
///
/// Emission path: `src/types/<snake_name>.rs`. The codegen orchestrator
/// wraps the file's body in `@generated:begin/end` markers, so users can
/// add `impl` blocks below the generated region and they survive regen.
pub struct LanguageStruct {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguageStruct {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.struct").expect("language.struct id is static and valid"),
            display: TemplateDisplay::new(
                "Struct",
                "Language",
                "Define a Rust struct visually — name, derives, visibility, fields. Emits to src/types/.",
            ),
            // Language nodes are definitional, not data-flow. They have no
            // runtime ports; other nodes reference them by name in their
            // own type fields. Same shape as core.dto.
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(LanguageStructConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguageStruct {
    fn id(&self) -> &TemplateId {
        &self.id
    }
    fn display(&self) -> &TemplateDisplay {
        &self.display
    }
    fn input_ports(&self) -> &[PortSpec] {
        &self.inputs
    }
    fn output_ports(&self) -> &[PortSpec] {
        &self.outputs
    }
    fn config_schema(&self) -> &Value {
        &self.schema
    }
    /// Struct is a *type definition*, not a runtime computation — it
    /// emits via `emit_schema` and reports `CodegenMode::Codegen`. Mirrors
    /// `core.dto`.
    fn codegen_mode(&self) -> CodegenMode {
        CodegenMode::Codegen
    }
    /// PassThrough: there's no async work to instrument; the debug bridge
    /// only records traversal of definitional nodes.
    fn debug_bridge(&self) -> DebugBridgeKind {
        DebugBridgeKind::PassThrough
    }

    fn emit_runtime(
        &self,
        _ctx: &CodegenCtx<'_>,
    ) -> Result<RuntimeEmission, TemplateError> {
        // No runtime — see codegen_mode().
        Ok(RuntimeEmission {
            items: vec![],
            dependencies: vec![],
            debug_site: None,
        })
    }

    fn emit_schema(&self, ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        let config: LanguageStructConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let name_ident = format_ident!("{}", config.name);
        let vis_tokens = config.visibility.as_tokens();
        let derive_tokens = derive_attr(&config.derives);

        let field_tokens: Vec<TokenStream> = config
            .fields
            .iter()
            .map(|f| {
                let field_ident = format_ident!("{}", f.name);
                let field_vis = f.visibility.as_tokens();
                // Pre-validated by `config.validate()`, so this parse_str
                // cannot fail here. Re-running it returns Tokens we can
                // splice into `quote!`.
                let ty: syn::Type =
                    parse_str(&f.ty).expect("type parsed in validate()");
                let ty_tokens = ty.into_token_stream();
                quote! { #field_vis #field_ident: #ty_tokens }
            })
            .collect();

        let (generics_tokens, where_tokens) = format_generics(&config.generics, config.where_clause.as_deref())?;

        // Empty field list emits unit-style `struct X;` not `struct X {}`,
        // matching the more idiomatic form for a marker type.
        let body = if field_tokens.is_empty() {
            quote! { #derive_tokens #vis_tokens struct #name_ident #generics_tokens #where_tokens; }
        } else {
            quote! {
                #derive_tokens
                #vis_tokens struct #name_ident #generics_tokens #where_tokens {
                    #(#field_tokens,)*
                }
            }
        };

        let formatted = format_tokens("language.struct", body)?;
        let snake = to_snake_case(&config.name);

        // Add serde to the dep hint set if any chosen derive requires it.
        let mut deps: Vec<(String, String)> = Vec::new();
        if config.derives.iter().any(|d| d.needs_serde()) {
            deps.push(("serde".to_string(), "1".to_string()));
        }

        Ok(SchemaEmission {
            items: vec![EmittedItem {
                module_path: format!("types/{}.rs", snake),
                source: formatted,
            }],
            dependencies: deps,
        })
    }
}

// ---------------------------------------------------------------------------
// language.enum
// ---------------------------------------------------------------------------

/// Per-instance config for [`LanguageEnum`]. Reuses [`Visibility`] and
/// [`DeriveKind`] from the struct template.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguageEnumConfig {
    /// CamelCase Rust identifier — e.g. `Status`, `OrderState`.
    pub(super) name: String,
    #[serde(default)]
    pub(super) visibility: Visibility,
    #[serde(default)]
    pub(super) derives: Vec<DeriveKind>,
    /// Ordered variant list. May be empty — emits `pub enum X {}`
    /// (never-instantiable, which is a legitimate Rust shape).
    #[serde(default)]
    pub(super) variants: Vec<EnumVariant>,
    /// Generic parameters (lifetimes, then types)
    #[serde(default)]
    pub(super) generics: Vec<GenericParam>,
    /// Optional where clause for generic constraints.
    #[serde(default)]
    pub(super) where_clause: Option<String>,
}

/// One variant in an enum definition. Externally tagged: the JSON looks
/// like `{ "name": "Foo", "payload": { "kind": "tuple", "types": [...] } }`.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct EnumVariant {
    /// CamelCase variant identifier.
    pub(super) name: String,
    /// Payload shape. Defaults to `Unit`, producing a bare `Foo` variant.
    #[serde(default)]
    pub(super) payload: VariantPayload,
}

/// Payload shape for one enum variant.
///
/// Externally tagged via `#[serde(tag = "kind", ...)]` so the JSON wire
/// shape is `{"kind":"unit"}` / `{"kind":"tuple","types":[...]}` /
/// `{"kind":"struct","fields":[...]}` — easy for the frontend form to
/// render as a radio-group + conditional field list.
#[derive(Debug, Clone, JsonSchema, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum VariantPayload {
    /// `Foo` — bare variant, no data.
    #[default]
    Unit,
    /// `Foo(T1, T2)` — positional tuple payload.
    Tuple { types: Vec<String> },
    /// `Foo { a: T1, b: T2 }` — named-field payload (reuses [`StructField`]).
    Struct { fields: Vec<StructField> },
}

impl LanguageEnumConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        if !is_valid_camel_case_ident(&self.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.enum: name {:?} is not a CamelCase Rust identifier",
                self.name
            )));
        }
        if parse_str::<syn::Ident>(&self.name).is_err() {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.enum: name {:?} is a Rust reserved word or otherwise not a valid identifier",
                self.name
            )));
        }

        let mut seen_derives: HashSet<DeriveKind> = HashSet::with_capacity(self.derives.len());
        for d in &self.derives {
            if !seen_derives.insert(*d) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.enum: duplicate derive {:?}",
                    d
                )));
            }
        }

        let mut seen_variants: HashSet<&str> = HashSet::with_capacity(self.variants.len());
        for (i, v) in self.variants.iter().enumerate() {
            if !is_valid_camel_case_ident(&v.name) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.enum: variant {i} name {:?} is not a CamelCase Rust identifier",
                    v.name
                )));
            }
            if parse_str::<syn::Ident>(&v.name).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.enum: variant {i} name {:?} is a Rust reserved word",
                    v.name
                )));
            }
            if !seen_variants.insert(v.name.as_str()) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.enum: duplicate variant name {:?}",
                    v.name
                )));
            }

            match &v.payload {
                VariantPayload::Unit => {}
                VariantPayload::Tuple { types } => {
                    for (j, ty) in types.iter().enumerate() {
                        if parse_str::<syn::Type>(ty).is_err() {
                            return Err(TemplateError::ConfigMismatch(format!(
                                "language.enum: variant {i} ({:?}) tuple element {j} ({:?}) is not a valid Rust type",
                                v.name, ty
                            )));
                        }
                    }
                }
                VariantPayload::Struct { fields } => {
                    let mut seen_fields: HashSet<&str> = HashSet::with_capacity(fields.len());
                    for (j, f) in fields.iter().enumerate() {
                        if !is_valid_snake_case_ident(&f.name) {
                            return Err(TemplateError::ConfigMismatch(format!(
                                "language.enum: variant {i} ({:?}) struct field {j} name {:?} is not snake_case",
                                v.name, f.name
                            )));
                        }
                        if parse_str::<syn::Ident>(&f.name).is_err() {
                            return Err(TemplateError::ConfigMismatch(format!(
                                "language.enum: variant {i} ({:?}) struct field {j} name {:?} is a Rust reserved word",
                                v.name, f.name
                            )));
                        }
                        if !seen_fields.insert(f.name.as_str()) {
                            return Err(TemplateError::ConfigMismatch(format!(
                                "language.enum: variant {i} ({:?}) duplicate field name {:?}",
                                v.name, f.name
                            )));
                        }
                        if parse_str::<syn::Type>(&f.ty).is_err() {
                            return Err(TemplateError::ConfigMismatch(format!(
                                "language.enum: variant {i} ({:?}) struct field {:?} has invalid type {:?}",
                                v.name, f.name, f.ty
                            )));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// `language.enum` — emits `[visibility] enum Name { ... }` with Unit /
/// Tuple / Struct variant payloads. Empty variant list is allowed (and
/// emits `enum Name {}`).
///
/// Mirrors the design of [`LanguageStruct`]: `CodegenMode::Codegen`,
/// PassThrough debug bridge, output at `src/types/<snake_name>.rs`. Sharing
/// the type module keeps the user-project tree predictable — every Rust
/// type definition produced by a language node lands in one place.
pub struct LanguageEnum {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguageEnum {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.enum").expect("language.enum id is static and valid"),
            display: TemplateDisplay::new(
                "Enum",
                "Language",
                "Define a Rust enum visually — name, derives, visibility, and Unit/Tuple/Struct-payload variants. Emits to src/types/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(LanguageEnumConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguageEnum {
    fn id(&self) -> &TemplateId {
        &self.id
    }
    fn display(&self) -> &TemplateDisplay {
        &self.display
    }
    fn input_ports(&self) -> &[PortSpec] {
        &self.inputs
    }
    fn output_ports(&self) -> &[PortSpec] {
        &self.outputs
    }
    fn config_schema(&self) -> &Value {
        &self.schema
    }
    fn codegen_mode(&self) -> CodegenMode {
        CodegenMode::Codegen
    }
    fn debug_bridge(&self) -> DebugBridgeKind {
        DebugBridgeKind::PassThrough
    }

    fn emit_runtime(
        &self,
        _ctx: &CodegenCtx<'_>,
    ) -> Result<RuntimeEmission, TemplateError> {
        Ok(RuntimeEmission {
            items: vec![],
            dependencies: vec![],
            debug_site: None,
        })
    }

    fn emit_schema(&self, ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        let config: LanguageEnumConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let name_ident = format_ident!("{}", config.name);
        let vis_tokens = config.visibility.as_tokens();
        let derive_tokens = derive_attr(&config.derives);

        let variant_tokens: Vec<TokenStream> = config
            .variants
            .iter()
            .map(|v| {
                let v_ident = format_ident!("{}", v.name);
                match &v.payload {
                    VariantPayload::Unit => quote! { #v_ident },
                    VariantPayload::Tuple { types } if types.is_empty() => {
                        // Empty tuple payload becomes a unit variant — avoids
                        // emitting `Foo()` which is legal Rust but rarely the
                        // intent. Frontends should default to Unit.
                        quote! { #v_ident }
                    }
                    VariantPayload::Tuple { types } => {
                        let ty_tokens: Vec<TokenStream> = types
                            .iter()
                            .map(|t| {
                                let ty: syn::Type =
                                    parse_str(t).expect("type validated in validate()");
                                ty.into_token_stream()
                            })
                            .collect();
                        quote! { #v_ident( #(#ty_tokens),* ) }
                    }
                    VariantPayload::Struct { fields } if fields.is_empty() => {
                        // Same flattening rule: empty struct payload → unit.
                        quote! { #v_ident }
                    }
                    VariantPayload::Struct { fields } => {
                        let field_tokens: Vec<TokenStream> = fields
                            .iter()
                            .map(|f| {
                                let f_ident = format_ident!("{}", f.name);
                                let ty: syn::Type =
                                    parse_str(&f.ty).expect("type validated in validate()");
                                let ty_tokens = ty.into_token_stream();
                                quote! { #f_ident: #ty_tokens }
                            })
                            .collect();
                        quote! { #v_ident { #(#field_tokens),* } }
                    }
                }
            })
            .collect();

        let (generics_tokens, where_tokens) = format_generics(&config.generics, config.where_clause.as_deref())?;

        let body = quote! {
            #derive_tokens
            #vis_tokens enum #name_ident #generics_tokens #where_tokens {
                #(#variant_tokens,)*
            }
        };

        let formatted = format_tokens("language.enum", body)?;
        let snake = to_snake_case(&config.name);

        let mut deps: Vec<(String, String)> = Vec::new();
        if config.derives.iter().any(|d| d.needs_serde()) {
            deps.push(("serde".to_string(), "1".to_string()));
        }

        Ok(SchemaEmission {
            items: vec![EmittedItem {
                module_path: format!("types/{}.rs", snake),
                source: formatted,
            }],
            dependencies: deps,
        })
    }
}

// ---------------------------------------------------------------------------
// language.fn
// ---------------------------------------------------------------------------

/// Per-instance config for [`LanguageFn`]. Reuses [`Visibility`] from
/// the shared helpers.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguageFnConfig {
    /// snake_case Rust function identifier.
    pub(super) name: String,
    #[serde(default)]
    pub(super) visibility: Visibility,
    /// If true, the function is emitted as `async fn`. Tokio nodes (S15b)
    /// will require this to be true on their host function.
    #[serde(default)]
    pub(super) is_async: bool,
    /// If true, the function is emitted as `unsafe fn`. Combinable with
    /// `is_async` (Rust permits `async unsafe fn`).
    #[serde(default)]
    pub(super) is_unsafe: bool,
    /// Generic parameter list. Each entry becomes `<Name: Bound>`. Lifetime
    /// parameters are deferred to S15d.
    #[serde(default)]
    pub(super) generics: Vec<GenericParam>,
    /// Positional parameter list (name + type). `self` receivers are not
    /// supported at this layer — methods will land on a future Impl node.
    #[serde(default)]
    pub(super) params: Vec<FnParam>,
    /// Optional return type. Absent → emit no `->` clause.
    #[serde(default)]
    pub(super) return_type: Option<String>,
    /// Free-form Rust body. Wrapped in `{ ... }` before parsing as a
    /// `syn::Block`, so the user does NOT supply outer braces. Empty
    /// string → empty block. When the function declares a non-`()` return
    /// type and the body is empty, the emission inserts `unimplemented!()`
    /// so cargo's error message is "called unimplemented" at runtime
    /// rather than "expected X, found ()" at compile time — clearer
    /// signal that the user has a stub to fill in.
    #[serde(default)]
    pub(super) body: String,
    /// Optional where clause for generic constraints.
    #[serde(default)]
    pub(super) where_clause: Option<String>,
}

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct GenericParam {
    /// CamelCase Rust type-parameter name (e.g. `T`, `Item`).
    pub(super) name: String,
    /// Optional trait bound. Accepts compound bounds like `Send + Sync`
    /// or lifetime+trait combos — anything `syn::TypeParamBound` parses.
    #[serde(default)]
    pub(super) bound: Option<String>,
}

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct FnParam {
    /// snake_case parameter identifier.
    pub(super) name: String,
    /// Rust type literal — anything `syn::parse_str::<syn::Type>` accepts.
    pub(super) ty: String,
}

impl LanguageFnConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        if !is_valid_snake_case_ident(&self.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.fn: name {:?} is not a snake_case Rust identifier",
                self.name
            )));
        }
        if parse_str::<syn::Ident>(&self.name).is_err() {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.fn: name {:?} is a Rust reserved word or otherwise not a valid identifier",
                self.name
            )));
        }

        validate_generics_and_where(&self.generics, self.where_clause.as_deref(), "language.fn")?;

        // Positional parameters — snake_case + syn::Ident; type must parse.
        let mut seen_params: HashSet<&str> = HashSet::with_capacity(self.params.len());
        for (i, p) in self.params.iter().enumerate() {
            if !is_valid_snake_case_ident(&p.name) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.fn: param {i} name {:?} is not a snake_case Rust identifier",
                    p.name
                )));
            }
            if parse_str::<syn::Ident>(&p.name).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.fn: param {i} name {:?} is a Rust reserved word",
                    p.name
                )));
            }
            if !seen_params.insert(p.name.as_str()) {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.fn: duplicate param name {:?}",
                    p.name
                )));
            }
            if parse_str::<syn::Type>(&p.ty).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.fn: param {i} ({:?}) has invalid type {:?}",
                    p.name, p.ty
                )));
            }
        }

        // Return type — when supplied, must parse.
        if let Some(rt) = &self.return_type {
            if parse_str::<syn::Type>(rt).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.fn: return type {:?} is not a valid Rust type",
                    rt
                )));
            }
        }

        // Body — when non-empty, must parse as `syn::Block` once wrapped
        // in `{ ... }`. We check the wrapped form so the user supplies
        // *contents* of the block, not the braces.
        if !self.body.is_empty() {
            let wrapped = format!("{{ {} }}", self.body);
            if parse_str::<syn::Block>(&wrapped).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.fn: body did not parse as a Rust block: {:?}",
                    self.body
                )));
            }
        }

        Ok(())
    }
}

/// `language.fn` — emits `[vis] [async] [unsafe] fn name<G: B>(p: T) -> R { body }`
/// to `src/functions/<snake_name>.rs`.
///
/// Unlike Struct/Enum, this is `CodegenMode::Runtime` — the function is
/// real callable code, not a type definition. The body is a free-form
/// string the user writes (S15f will replace this with a Monaco editor
/// in the config drawer). Validation parses the wrapped body as a
/// `syn::Block` so syntactic errors surface here rather than as opaque
/// cargo errors later.
pub struct LanguageFn {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguageFn {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.fn").expect("language.fn id is static and valid"),
            display: TemplateDisplay::new(
                "Function",
                "Language",
                "Define a Rust function visually — signature (async/unsafe, generics, params, return) and free-form body. Emits to src/functions/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(LanguageFnConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguageFn {
    fn id(&self) -> &TemplateId {
        &self.id
    }
    fn display(&self) -> &TemplateDisplay {
        &self.display
    }
    fn input_ports(&self) -> &[PortSpec] {
        &self.inputs
    }
    fn output_ports(&self) -> &[PortSpec] {
        &self.outputs
    }
    fn config_schema(&self) -> &Value {
        &self.schema
    }
    fn codegen_mode(&self) -> CodegenMode {
        CodegenMode::Runtime
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: LanguageFnConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let name_ident = format_ident!("{}", config.name);
        let vis_tokens = config.visibility.as_tokens();

        // Async / unsafe combine cleanly — `async unsafe fn` is legal.
        let async_kw = if config.is_async { quote! { async } } else { quote! {} };
        let unsafe_kw = if config.is_unsafe { quote! { unsafe } } else { quote! {} };

        let (generics_tokens, where_tokens) = format_generics(&config.generics, config.where_clause.as_deref())?;

        // Parameters: `name: Type` pairs.
        let param_tokens: Vec<TokenStream> = config
            .params
            .iter()
            .map(|p| {
                let p_ident = format_ident!("{}", p.name);
                let ty: syn::Type = parse_str(&p.ty).expect("type validated in validate()");
                let ty_tokens = ty.into_token_stream();
                quote! { #p_ident: #ty_tokens }
            })
            .collect();

        // Return type: `-> R` clause, or nothing.
        let return_tokens = match &config.return_type {
            Some(rt) => {
                let ty: syn::Type = parse_str(rt).expect("return type validated in validate()");
                let ty_tokens = ty.into_token_stream();
                quote! { -> #ty_tokens }
            }
            None => quote! {},
        };

        // Body: empty → `{}` for the no-return case, `{ unimplemented!() }`
        // when there's a declared return type (gives the user a usable stub
        // and a clear runtime panic message rather than a confusing compile
        // error about expected type). Non-empty bodies are parsed as a
        // syn::Block in validate(); we re-parse here to splice as tokens.
        let body_tokens = if config.body.is_empty() {
            if config.return_type.is_some() {
                quote! { { unimplemented!() } }
            } else {
                quote! { {} }
            }
        } else {
            let wrapped = format!("{{ {} }}", config.body);
            let block: syn::Block =
                parse_str(&wrapped).expect("body validated in validate()");
            block.into_token_stream()
        };

        let tokens = quote! {
            #vis_tokens #async_kw #unsafe_kw fn #name_ident #generics_tokens ( #(#param_tokens),* ) #return_tokens #where_tokens #body_tokens
        };

        let formatted = format_tokens("language.fn", tokens)?;
        let snake = to_snake_case(&config.name);

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("functions/{}.rs", snake),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::types::{Graph, Node, NodeId, Position, GRAPH_SCHEMA_VERSION};
    use crate::templates::TemplateId;
    use serde_json::json;
    use std::path::PathBuf;

    /// Build a CodegenCtx with a single configured node so emission can
    /// be exercised in isolation. The graph and output_root are stubbed
    /// because language templates don't read either (yet).
    fn ctx(template_id: &str, config: Value) -> (PathBuf, Graph, Node) {
        let node = Node {
            id: NodeId("n1".to_string()),
            template_id: TemplateId::new(template_id).expect("valid id"),
            position: Position { x: 0.0, y: 0.0 },
            config,
            label: None,
        };
        let graph = Graph {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes: vec![node.clone()],
            edges: Vec::new(),
        };
        (PathBuf::from("/tmp/out"), graph, node)
    }

    fn emit(config: Value) -> Result<SchemaEmission, TemplateError> {
        let (root, graph, node) = ctx("language.struct", config);
        let template = LanguageStruct::new();
        template.emit_schema(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_enum(config: Value) -> Result<SchemaEmission, TemplateError> {
        let (root, graph, node) = ctx("language.enum", config);
        let template = LanguageEnum::new();
        template.emit_schema(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_fn(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("language.fn", config);
        let template = LanguageFn::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    #[test]
    fn test_language_struct_emits_minimal_unit_struct() {
        let out = emit(json!({ "name": "Marker" })).expect("emits");
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].module_path, "types/marker.rs");
        assert!(
            out.items[0].source.contains("pub struct Marker;"),
            "actual:\n{}",
            out.items[0].source
        );
        assert!(out.dependencies.is_empty());
    }

    #[test]
    fn test_language_struct_emits_named_fields_with_derives() {
        let out = emit(json!({
            "name": "User",
            "derives": ["Debug", "Clone", "Serialize", "Deserialize"],
            "fields": [
                { "name": "id", "ty": "u64" },
                { "name": "email", "ty": "String" },
                { "name": "tags", "ty": "Vec<String>" }
            ]
        }))
        .expect("emits");

        let src = &out.items[0].source;
        assert!(src.contains("pub struct User"), "actual:\n{src}");
        assert!(src.contains("pub id: u64"), "actual:\n{src}");
        assert!(src.contains("pub email: String"), "actual:\n{src}");
        assert!(src.contains("pub tags: Vec<String>"), "actual:\n{src}");
        assert!(
            src.contains("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"),
            "actual:\n{src}"
        );
        assert_eq!(out.dependencies, vec![("serde".to_string(), "1".to_string())]);
    }

    #[test]
    fn test_language_struct_emission_is_byte_stable() {
        let cfg = json!({
            "name": "Order",
            "derives": ["Debug", "Clone"],
            "fields": [
                { "name": "id", "ty": "u64" },
                { "name": "qty", "ty": "u32" }
            ]
        });
        let a = emit(cfg.clone()).expect("emits a").items[0].source.clone();
        let b = emit(cfg).expect("emits b").items[0].source.clone();
        assert_eq!(a, b, "emission must be byte-stable across calls");
    }

    #[test]
    fn test_language_struct_visibility_pub_crate() {
        let out = emit(json!({
            "name": "Internal",
            "visibility": "pub_crate"
        }))
        .expect("emits");
        assert!(
            out.items[0].source.contains("pub(crate) struct Internal;"),
            "actual:\n{}",
            out.items[0].source
        );
    }

    #[test]
    fn test_language_struct_visibility_private() {
        let out = emit(json!({
            "name": "Internal",
            "visibility": "private"
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(
            src.contains("struct Internal;") && !src.contains("pub struct"),
            "actual:\n{src}"
        );
    }

    #[test]
    fn test_language_struct_rejects_bad_camel_case_name() {
        let err = emit(json!({ "name": "lowercase_start" })).unwrap_err();
        assert!(matches!(err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_struct_rejects_empty_name() {
        let err = emit(json!({ "name": "" })).unwrap_err();
        assert!(matches!(err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_struct_rejects_duplicate_field_names() {
        let err = emit(json!({
            "name": "Dup",
            "fields": [
                { "name": "x", "ty": "u8" },
                { "name": "x", "ty": "u16" }
            ]
        }))
        .unwrap_err();
        assert!(matches!(err, TemplateError::ConfigMismatch(msg) if msg.contains("duplicate")));
    }

    #[test]
    fn test_language_struct_rejects_unparseable_field_type() {
        let err = emit(json!({
            "name": "Bad",
            "fields": [
                { "name": "x", "ty": "Vec<{" }
            ]
        }))
        .unwrap_err();
        assert!(matches!(err, TemplateError::ConfigMismatch(_)));
    }

    #[test]
    fn test_language_struct_rejects_bad_snake_case_field_name() {
        let err = emit(json!({
            "name": "Bad",
            "fields": [
                { "name": "CamelField", "ty": "u8" }
            ]
        }))
        .unwrap_err();
        assert!(matches!(err, TemplateError::ConfigMismatch(_)));
    }

    #[test]
    fn test_language_struct_no_serde_dep_without_serde_derive() {
        let out = emit(json!({
            "name": "NoSerde",
            "derives": ["Debug", "Clone"]
        }))
        .expect("emits");
        assert!(out.dependencies.is_empty());
    }

    #[test]
    fn test_language_struct_emitted_source_compiles_through_syn_round_trip() {
        // Hard correctness check: the formatted output must parse back as
        // a `syn::File`. Catches accidental whitespace bugs and any future
        // codegen change that produces non-Rust output.
        let out = emit(json!({
            "name": "Complex",
            "derives": ["Debug"],
            "fields": [
                { "name": "a", "ty": "Option<Vec<u8>>" },
                { "name": "b", "ty": "std::collections::HashMap<String, u64>" }
            ]
        }))
        .expect("emits");
        let _: syn::File =
            syn::parse_str(&out.items[0].source).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_struct_rejects_reserved_keyword_as_struct_name() {
        // `Self` passes the CamelCase regex but is a reserved keyword
        // that would panic `format_ident!` at emit time. Must be caught
        // by the second-line `syn::Ident` check. `Super` is NOT a keyword
        // in name position (only the lowercase `super` path qualifier is).
        let err = emit(json!({ "name": "Self" })).unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("reserved word")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_struct_rejects_reserved_keyword_as_field_name() {
        // `type` passes the snake_case regex but is a Rust keyword.
        for kw in &["type", "fn", "match", "let", "self"] {
            let err = emit(json!({
                "name": "Holder",
                "fields": [{ "name": kw, "ty": "u8" }]
            }))
            .unwrap_err();
            assert!(
                matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("reserved word")),
                "got {err:?} for {kw}"
            );
        }
    }

    #[test]
    fn test_language_struct_rejects_duplicate_derives() {
        let err = emit(json!({
            "name": "Dup",
            "derives": ["Copy", "Copy"]
        }))
        .unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("duplicate derive")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_struct_accepts_reference_and_fn_pointer_types() {
        // syn::Type covers more than the obvious — references with explicit
        // lifetimes and fn pointers must round-trip cleanly.
        let out = emit(json!({
            "name": "Exotic",
            "fields": [
                { "name": "name", "ty": "&'static str" },
                { "name": "cb",   "ty": "fn(u8) -> bool" }
            ]
        }))
        .expect("emits");
        let src = &out.items[0].source;
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
        assert!(src.contains("&'static str"), "actual:\n{src}");
        assert!(src.contains("fn(u8) -> bool"), "actual:\n{src}");
    }

    #[test]
    fn test_camel_case_validator() {
        assert!(is_valid_camel_case_ident("Foo"));
        assert!(is_valid_camel_case_ident("FooBar"));
        assert!(is_valid_camel_case_ident("Foo2"));
        assert!(!is_valid_camel_case_ident(""));
        assert!(!is_valid_camel_case_ident("foo"));
        assert!(!is_valid_camel_case_ident("Foo_Bar")); // underscore disallowed
        assert!(!is_valid_camel_case_ident("Foo-Bar"));
    }

    #[test]
    fn test_snake_case_validator() {
        assert!(is_valid_snake_case_ident("x"));
        assert!(is_valid_snake_case_ident("foo_bar"));
        assert!(is_valid_snake_case_ident("_private"));
        assert!(is_valid_snake_case_ident("a1"));
        assert!(!is_valid_snake_case_ident(""));
        assert!(!is_valid_snake_case_ident("Foo"));
        assert!(!is_valid_snake_case_ident("1a"));
        assert!(!is_valid_snake_case_ident("foo-bar"));
    }

    // -----------------------------------------------------------------------
    // language.enum tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_language_enum_emits_simple_unit_variants() {
        let out = emit_enum(json!({
            "name": "Status",
            "variants": [
                { "name": "Active" },
                { "name": "Inactive" }
            ]
        }))
        .expect("emits");
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].module_path, "types/status.rs");
        let src = &out.items[0].source;
        assert!(src.contains("pub enum Status"), "actual:\n{src}");
        assert!(src.contains("Active"), "actual:\n{src}");
        assert!(src.contains("Inactive"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_enum_emits_tuple_variant() {
        let out = emit_enum(json!({
            "name": "Msg",
            "variants": [
                { "name": "Ping" },
                {
                    "name": "Data",
                    "payload": { "kind": "tuple", "types": ["String", "u64"] }
                }
            ]
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("Data(String, u64)"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_enum_emits_struct_variant() {
        let out = emit_enum(json!({
            "name": "Event",
            "variants": [{
                "name": "Click",
                "payload": {
                    "kind": "struct",
                    "fields": [
                        { "name": "x", "ty": "i32" },
                        { "name": "y", "ty": "i32" }
                    ]
                }
            }]
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("Click {") || src.contains("Click{"), "actual:\n{src}");
        assert!(src.contains("x: i32"), "actual:\n{src}");
        assert!(src.contains("y: i32"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_enum_serde_derives_add_dep() {
        let out = emit_enum(json!({
            "name": "Tag",
            "derives": ["Debug", "Serialize", "Deserialize"],
            "variants": [{ "name": "One" }, { "name": "Two" }]
        }))
        .expect("emits");
        assert_eq!(out.dependencies, vec![("serde".to_string(), "1".to_string())]);
        let src = &out.items[0].source;
        assert!(
            src.contains("#[derive(Debug, serde::Serialize, serde::Deserialize)]"),
            "actual:\n{src}"
        );
    }

    #[test]
    fn test_language_enum_empty_variants_emits_empty_block() {
        // Empty enums are legal Rust (never-instantiable) — supported.
        let out = emit_enum(json!({ "name": "Void" })).expect("emits");
        let src = &out.items[0].source;
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
        assert!(src.contains("pub enum Void"), "actual:\n{src}");
    }

    #[test]
    fn test_language_enum_emission_byte_stable() {
        let cfg = json!({
            "name": "Color",
            "derives": ["Debug", "Clone"],
            "variants": [
                { "name": "Red" },
                { "name": "Green" },
                {
                    "name": "Custom",
                    "payload": { "kind": "tuple", "types": ["u8", "u8", "u8"] }
                }
            ]
        });
        let a = emit_enum(cfg.clone()).expect("emits a").items[0].source.clone();
        let b = emit_enum(cfg).expect("emits b").items[0].source.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn test_language_enum_rejects_duplicate_variant_names() {
        let err = emit_enum(json!({
            "name": "Dup",
            "variants": [{ "name": "A" }, { "name": "A" }]
        }))
        .unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("duplicate variant")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_enum_rejects_reserved_keyword_variant_name() {
        // CamelCase regex accepts `Self`; syn::Ident parse rejects it.
        let err = emit_enum(json!({
            "name": "Holder",
            "variants": [{ "name": "Self" }]
        }))
        .unwrap_err();
        assert!(matches!(&err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_enum_rejects_bad_tuple_type() {
        let err = emit_enum(json!({
            "name": "Bad",
            "variants": [{
                "name": "Boom",
                "payload": { "kind": "tuple", "types": ["Vec<{"] }
            }]
        }))
        .unwrap_err();
        assert!(matches!(&err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_enum_rejects_struct_variant_duplicate_field_names() {
        let err = emit_enum(json!({
            "name": "Bad",
            "variants": [{
                "name": "Pair",
                "payload": {
                    "kind": "struct",
                    "fields": [
                        { "name": "x", "ty": "u8" },
                        { "name": "x", "ty": "u8" }
                    ]
                }
            }]
        }))
        .unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("duplicate")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_enum_rejects_duplicate_derives() {
        let err = emit_enum(json!({
            "name": "Dup",
            "derives": ["Debug", "Debug"],
            "variants": []
        }))
        .unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("duplicate derive")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_enum_rejects_bad_name() {
        let err = emit_enum(json!({ "name": "lowercase" })).unwrap_err();
        assert!(matches!(&err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_enum_rejects_struct_variant_reserved_keyword_field() {
        let err = emit_enum(json!({
            "name": "E",
            "variants": [{
                "name": "V",
                "payload": {
                    "kind": "struct",
                    "fields": [{ "name": "type", "ty": "u8" }]
                }
            }]
        }))
        .unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("reserved word")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_enum_rejects_struct_variant_unparseable_field_type() {
        let err = emit_enum(json!({
            "name": "E",
            "variants": [{
                "name": "V",
                "payload": {
                    "kind": "struct",
                    "fields": [{ "name": "x", "ty": "Vec<{" }]
                }
            }]
        }))
        .unwrap_err();
        assert!(matches!(&err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_enum_empty_struct_payload_flattens_to_unit_variant() {
        let out = emit_enum(json!({
            "name": "E",
            "variants": [{
                "name": "X",
                "payload": { "kind": "struct", "fields": [] }
            }]
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(!src.contains("X {") && !src.contains("X{"), "should flatten\nactual:\n{src}");
        assert!(src.contains("X"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_enum_empty_tuple_payload_flattens_to_unit_variant() {
        // `{"kind":"tuple","types":[]}` is a degenerate input — emit as Unit
        // rather than `Foo()` which is rarely the intent.
        let out = emit_enum(json!({
            "name": "E",
            "variants": [{
                "name": "X",
                "payload": { "kind": "tuple", "types": [] }
            }]
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(!src.contains("X()"), "should not emit X()\nactual:\n{src}");
        assert!(src.contains("X"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    // -----------------------------------------------------------------------
    // language.fn tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_language_fn_emits_minimal_empty_function() {
        let out = emit_fn(json!({ "name": "noop" })).expect("emits");
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].module_path, "functions/noop.rs");
        let src = &out.items[0].source;
        assert!(src.contains("pub fn noop()"), "actual:\n{src}");
        assert!(src.contains("{}") || src.contains("{ }"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
        // No deps for a parameterless sync fn with no return type.
        assert!(out.dependencies.is_empty());
    }

    #[test]
    fn test_language_fn_emits_async_with_params_and_return() {
        let out = emit_fn(json!({
            "name": "handle",
            "is_async": true,
            "params": [
                { "name": "id",   "ty": "u64" },
                { "name": "name", "ty": "String" }
            ],
            "return_type": "Result<(), std::io::Error>",
            "body": "Ok(())"
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("pub async fn handle"), "actual:\n{src}");
        assert!(src.contains("id: u64"), "actual:\n{src}");
        assert!(src.contains("name: String"), "actual:\n{src}");
        assert!(src.contains("-> Result<(), std::io::Error>"), "actual:\n{src}");
        assert!(src.contains("Ok(())"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_fn_emits_generic_with_bound() {
        let out = emit_fn(json!({
            "name": "show",
            "generics": [
                { "name": "T", "bound": "std::fmt::Display + Send" }
            ],
            "params": [{ "name": "x", "ty": "T" }],
            "body": "let _ = x;"
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("fn show<T: std::fmt::Display + Send>"), "actual:\n{src}");
        assert!(src.contains("x: T"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_fn_unsafe_keyword_emitted() {
        let out = emit_fn(json!({
            "name": "danger",
            "is_unsafe": true,
            "body": ""
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("pub unsafe fn danger"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_fn_async_unsafe_combined_is_legal() {
        let out = emit_fn(json!({
            "name": "danger",
            "is_async": true,
            "is_unsafe": true
        }))
        .expect("emits");
        let src = &out.items[0].source;
        // prettyplease keeps the documented ordering `async unsafe fn`.
        assert!(
            src.contains("pub async unsafe fn danger"),
            "actual:\n{src}"
        );
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_fn_empty_body_with_return_type_emits_unimplemented_stub() {
        // Documented contract: declared return type + empty body →
        // `unimplemented!()` so cargo errors are runtime "called
        // unimplemented" rather than compile-time "expected X found ()".
        let out = emit_fn(json!({
            "name": "todo",
            "return_type": "u32",
            "body": ""
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("unimplemented!()"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_fn_visibility_private_emits_no_pub() {
        let out = emit_fn(json!({
            "name": "helper",
            "visibility": "private"
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(!src.contains("pub fn helper"), "should be private\n{src}");
        assert!(src.contains("fn helper"), "actual:\n{src}");
    }

    #[test]
    fn test_language_fn_emission_is_byte_stable() {
        let cfg = json!({
            "name": "stable",
            "is_async": true,
            "params": [{ "name": "x", "ty": "u32" }],
            "return_type": "u32",
            "body": "x + 1"
        });
        let a = emit_fn(cfg.clone()).expect("emits a").items[0].source.clone();
        let b = emit_fn(cfg).expect("emits b").items[0].source.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn test_language_fn_rejects_non_snake_case_name() {
        let err = emit_fn(json!({ "name": "BadName" })).unwrap_err();
        assert!(matches!(&err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_fn_rejects_reserved_keyword_name() {
        // `type` is snake_case-shaped but a reserved keyword — must be
        // caught by the syn::Ident parse, not by the regex.
        let err = emit_fn(json!({ "name": "type" })).unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("reserved word")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_fn_rejects_reserved_keyword_param_name() {
        let err = emit_fn(json!({
            "name": "f",
            "params": [{ "name": "type", "ty": "u8" }]
        }))
        .unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("reserved word")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_fn_rejects_duplicate_param_names() {
        let err = emit_fn(json!({
            "name": "f",
            "params": [
                { "name": "x", "ty": "u8" },
                { "name": "x", "ty": "u16" }
            ]
        }))
        .unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("duplicate param")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_fn_rejects_unparseable_param_type() {
        let err = emit_fn(json!({
            "name": "f",
            "params": [{ "name": "x", "ty": "Vec<{" }]
        }))
        .unwrap_err();
        assert!(matches!(&err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_fn_rejects_unparseable_return_type() {
        let err = emit_fn(json!({
            "name": "f",
            "return_type": "Vec<{"
        }))
        .unwrap_err();
        assert!(matches!(&err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_fn_rejects_unparseable_body() {
        let err = emit_fn(json!({
            "name": "f",
            "body": "let x = ;" // syntactically broken
        }))
        .unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("body")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_fn_rejects_duplicate_generic_param_names() {
        let err = emit_fn(json!({
            "name": "f",
            "generics": [
                { "name": "T" },
                { "name": "T", "bound": "Send" }
            ]
        }))
        .unwrap_err();
        assert!(
            matches!(&err, TemplateError::ConfigMismatch(m) if m.contains("duplicate generic")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_language_fn_rejects_invalid_generic_bound() {
        let err = emit_fn(json!({
            "name": "f",
            "generics": [{ "name": "T", "bound": "Send + + Sync" }]
        }))
        .unwrap_err();
        assert!(matches!(&err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_fn_accepts_bare_and_mixed_generics() {
        // Cover the `None` arm of the per-generic closure plus the
        // mixed-list shape — `<T, U: Display>`.
        let out = emit_fn(json!({
            "name": "mixed",
            "generics": [
                { "name": "T" },
                { "name": "U", "bound": "std::fmt::Display" }
            ],
            "params": [
                { "name": "a", "ty": "T" },
                { "name": "b", "ty": "U" }
            ]
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("fn mixed<T, U: std::fmt::Display>"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_fn_accepts_lifetime_bound() {
        let out = emit_fn(json!({
            "name": "borrowing",
            "generics": [{ "name": "T", "bound": "'static" }],
            "params": [{ "name": "x", "ty": "T" }]
        }))
        .expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("fn borrowing<T: 'static>"), "actual:\n{src}");
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
    }

    #[test]
    fn test_language_fn_rejects_empty_bound_string() {
        // Empty bound after `T:` is degenerate — reject so the user
        // gets a clear template error instead of a confusing cargo error.
        let err = emit_fn(json!({
            "name": "f",
            "generics": [{ "name": "T", "bound": "" }]
        }))
        .unwrap_err();
        assert!(matches!(&err, TemplateError::ConfigMismatch(_)), "got {err:?}");
    }

    #[test]
    fn test_language_fn_accepts_multi_statement_body() {
        let out = emit_fn(json!({
            "name": "multi",
            "return_type": "u32",
            "body": "let x = 1; let y = 2; x + y"
        }))
        .expect("emits");
        let src = &out.items[0].source;
        let _: syn::File = syn::parse_str(src).expect("emitted source must reparse");
        assert!(src.contains("let x = 1"), "actual:\n{src}");
        assert!(src.contains("x + y"), "actual:\n{src}");
    }

    #[test]
    fn test_language_clone_emits_valid_clone_code() {
        let (root, graph, node) = ctx("language.clone", json!({ "name": "my_clone" }));
        let template = LanguageClone::new();
        let out = template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).expect("emits");
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].module_path, "functions/my_clone.rs");
        assert!(out.items[0].source.contains("pub fn my_clone()"));
    }

    #[test]
    fn test_language_struct_generics_and_lifetimes() {
        let (root, graph, node) = ctx(
            "language.struct",
            json!({
                "name": "User",
                "generics": [
                    { "name": "T", "bound": "Clone" },
                    { "name": "'a" }
                ],
                "where_clause": "T: Default",
                "fields": [
                    { "name": "data", "ty": "T", "visibility": "pub" },
                    { "name": "name", "ty": "&'a str", "visibility": "pub" }
                ]
            }),
        );
        let template = LanguageStruct::new();
        let out = template.emit_schema(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("struct User<'a, T: Clone>"), "actual:\n{src}");
        assert!(src.contains("T: Default"), "actual:\n{src}");
        assert!(src.contains("pub data: T"), "actual:\n{src}");
    }

    #[test]
    fn test_language_enum_generics_and_lifetimes() {
        let (root, graph, node) = ctx(
            "language.enum",
            json!({
                "name": "Status",
                "generics": [
                    { "name": "E" },
                    { "name": "'b", "bound": "'a" },
                    { "name": "'a" }
                ],
                "variants": [
                    { "name": "Pending" },
                    { "name": "Err", "payload": { "kind": "tuple", "types": ["E"] } }
                ]
            }),
        );
        let template = LanguageEnum::new();
        let out = template.emit_schema(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("enum Status<'b: 'a, 'a, E>"), "actual:\n{src}");
    }

    #[test]
    fn test_language_fn_lifetimes_sorted_first() {
        let (root, graph, node) = ctx(
            "language.fn",
            json!({
                "name": "process",
                "generics": [
                    { "name": "T", "bound": "std::fmt::Debug" },
                    { "name": "'a" }
                ],
                "where_clause": "T: Clone",
                "params": [
                    { "name": "item", "ty": "&'a T" }
                ],
                "return_type": "()",
                "body": "println!(\"{:?}\", item);"
            }),
        );
        let template = LanguageFn::new();
        let out = template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("fn process<'a, T: std::fmt::Debug>"), "actual:\n{src}");
        assert!(src.contains("T: Clone"), "actual:\n{src}");
    }

    #[test]
    fn test_language_if_else_emits_correct_rust() {
        let (root, graph, node) = ctx(
            "language.if_else",
            json!({
                "name": "check_age",
                "condition": "age >= 18",
                "true_expr": "\"adult\"",
                "false_expr": "\"minor\""
            }),
        );
        let template = LanguageIfElse::new();
        let out = template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("fn check_age()"), "actual:\n{src}");
        assert!(src.contains("if age >= 18"), "actual:\n{src}");
        assert!(src.contains("\"adult\""), "actual:\n{src}");
    }

    #[test]
    fn test_language_match_emits_correct_rust() {
        let (root, graph, node) = ctx(
            "language.match",
            json!({
                "name": "handle_state",
                "arms": [
                    { "pattern": "Some(x)", "expr": "x" },
                    { "pattern": "None", "expr": "0" }
                ]
            }),
        );
        let template = LanguageMatch::new();
        let out = template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("fn handle_state()"), "actual:\n{src}");
        assert!(src.contains("Some(x) => x"), "actual:\n{src}");
    }

    #[test]
    fn test_language_loop_emits_correct_rust() {
        let (root, graph, node) = ctx(
            "language.loop",
            json!({
                "name": "run_counter",
                "kind": "while",
                "condition": "x < 10",
                "body": "x += 1;"
            }),
        );
        let template = LanguageLoop::new();
        let out = template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).expect("emits");
        let src = &out.items[0].source;
        assert!(src.contains("fn run_counter()"), "actual:\n{src}");
        assert!(src.contains("while x < 10"), "actual:\n{src}");
        assert!(src.contains("x += 1;"), "actual:\n{src}");
    }
}

// ---------------------------------------------------------------------------
// language.clone
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguageCloneConfig {
    /// The snake_case Rust function name for the generated clone helper.
    pub(super) name: String,
}

pub struct LanguageClone {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguageClone {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.clone").expect("language.clone id is static and valid"),
            display: TemplateDisplay::new(
                "Clone",
                "Language",
                "Explicitly clones/copies a value to force ownership division on the canvas.",
            ),
            inputs: vec![PortSpec::single("input", "any", "Input value to clone.")],
            outputs: vec![PortSpec::single("output", "any", "Cloned output value.")],
            schema: serde_json::to_value(schemars::schema_for!(LanguageCloneConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguageClone {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: LanguageCloneConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        
        if !is_valid_snake_case_ident(&config.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.clone: name {:?} is not a snake_case Rust identifier",
                config.name
            )));
        }
        if parse_str::<syn::Ident>(&config.name).is_err() {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.clone: name {:?} is a Rust reserved word",
                config.name
            )));
        }

        // Find upstream variable name dynamically from the connected edge.
        let upstream_edge = ctx.graph.edges.iter()
            .find(|e| e.target == ctx.node.id && e.target_port == "input");
        
        let source = if let Some(edge) = upstream_edge {
            let up_var = crate::codegen::dataflow::get_value_var_name(&edge.source, &edge.source_port, ctx.graph);
            format!(
                "pub fn {}() -> impl Clone {{\n    /*[clone:{}]*/ {}\n}}\n",
                config.name, up_var, up_var
            )
        } else {
            format!(
                "pub fn {}() -> &'static str {{\n    \"unwired\"\n}}\n",
                config.name
            )
        };

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("functions/{}.rs", to_snake_case(&config.name)),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---------------------------------------------------------------------------
// language.if_else (S15c)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguageIfElseConfig {
    /// The snake_case Rust function name for the conditional check.
    pub(super) name: String,
    /// The conditional boolean expression (e.g. `x > 10`).
    pub(super) condition: String,
    /// Expression returned when the condition is true.
    pub(super) true_expr: String,
    /// Expression returned when the condition is false.
    pub(super) false_expr: String,
}

pub struct LanguageIfElse {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguageIfElse {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.if_else").expect("language.if_else id is static and valid"),
            display: TemplateDisplay::new(
                "If/Else",
                "Language",
                "Visual conditional branch node representing an if/else expression.",
            ),
            inputs: vec![PortSpec::single("input", "any", "Inputs used inside the expression.")],
            outputs: vec![PortSpec::single("output", "any", "The resulting value of the if/else branch.")],
            schema: serde_json::to_value(schemars::schema_for!(LanguageIfElseConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguageIfElse {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: LanguageIfElseConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        if !is_valid_snake_case_ident(&config.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.if_else: name {:?} is not a snake_case Rust identifier",
                config.name
            )));
        }

        let _cond: syn::Expr = parse_str(&config.condition)
            .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid condition syntax: {e}")))?;
        let _true: syn::Expr = parse_str(&config.true_expr)
            .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid true expression syntax: {e}")))?;
        let _false: syn::Expr = parse_str(&config.false_expr)
            .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid false expression syntax: {e}")))?;

        let upstream_edge = ctx.graph.edges.iter()
            .find(|e| e.target == ctx.node.id && e.target_port == "input");

        let source = if let Some(edge) = upstream_edge {
            let up_var = crate::codegen::dataflow::get_value_var_name(&edge.source, &edge.source_port, ctx.graph);
            format!(
                "pub fn {}({}: impl Clone) -> impl Clone {{\n    if {} {{\n        {}\n    }} else {{\n        {}\n    }}\n}}\n",
                config.name, up_var, config.condition, config.true_expr, config.false_expr
            )
        } else {
            format!(
                "pub fn {}() -> impl Clone {{\n    if {} {{\n        {}\n    }} else {{\n        {}\n    }}\n}}\n",
                config.name, config.condition, config.true_expr, config.false_expr
            )
        };

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("functions/{}.rs", to_snake_case(&config.name)),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---------------------------------------------------------------------------
// language.match (S15c)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct MatchArm {
    pub(super) pattern: String,
    pub(super) expr: String,
}

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguageMatchConfig {
    /// snake_case Rust function identifier.
    pub(super) name: String,
    /// Ordered arms for pattern matching.
    pub(super) arms: Vec<MatchArm>,
}

pub struct LanguageMatch {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguageMatch {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.match").expect("language.match id is static and valid"),
            display: TemplateDisplay::new(
                "Match",
                "Language",
                "Visual pattern matching node representing a match expression.",
            ),
            inputs: vec![PortSpec::single("input", "any", "Upstream value to match against.")],
            outputs: vec![PortSpec::single("output", "any", "The matched branch's value.")],
            schema: serde_json::to_value(schemars::schema_for!(LanguageMatchConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguageMatch {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: LanguageMatchConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        if !is_valid_snake_case_ident(&config.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.match: name {:?} is not a snake_case Rust identifier",
                config.name
            )));
        }

        if config.arms.is_empty() {
            return Err(TemplateError::ConfigMismatch("language.match: at least one match arm is required".to_string()));
        }

        for (i, arm) in config.arms.iter().enumerate() {
            let arm_str = format!("{} => {}", arm.pattern, arm.expr);
            let _parsed_arm: syn::Arm = parse_str(&arm_str)
                .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid match arm {i} syntax: {e}")))?;
        }

        let upstream_edge = ctx.graph.edges.iter()
            .find(|e| e.target == ctx.node.id && e.target_port == "input");

        let mut arms_source = String::new();
        for arm in &config.arms {
            arms_source.push_str(&format!("        {} => {},\n", arm.pattern, arm.expr));
        }

        let source = if let Some(edge) = upstream_edge {
            let up_var = crate::codegen::dataflow::get_value_var_name(&edge.source, &edge.source_port, ctx.graph);
            format!(
                "pub fn {}({}: impl Clone) -> impl Clone {{\n    match {} {{\n{}}}\n}}\n",
                config.name, up_var, up_var, arms_source
            )
        } else {
            format!(
                "pub fn {}() -> impl Clone {{\n    match \"\" {{\n{}}}\n}}\n",
                config.name, arms_source
            )
        };

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("functions/{}.rs", to_snake_case(&config.name)),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---------------------------------------------------------------------------
// language.loop (S15c)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, JsonSchema, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum LoopKind {
    Loop,
    While,
    For,
}

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguageLoopConfig {
    /// snake_case Rust function identifier.
    pub(super) name: String,
    /// loop, while, or for loop type.
    pub(super) kind: LoopKind,
    /// Loop conditional expression (e.g. `x < 10` or `item in items`).
    #[serde(default)]
    pub(super) condition: Option<String>,
    /// Body block statement inside the loop (e.g. `println!("{}", item);`).
    pub(super) body: String,
}

pub struct LanguageLoop {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguageLoop {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.loop").expect("language.loop id is static and valid"),
            display: TemplateDisplay::new(
                "Loop",
                "Language",
                "Visual iteration/looping constructs representing loop, while, or for.",
            ),
            inputs: vec![PortSpec::single("input", "any", "Inputs used inside the loop.")],
            outputs: vec![PortSpec::single("output", "any", "Loop return or yielded value.")],
            schema: serde_json::to_value(schemars::schema_for!(LanguageLoopConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguageLoop {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: LanguageLoopConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        if !is_valid_snake_case_ident(&config.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.loop: name {:?} is not a snake_case Rust identifier",
                config.name
            )));
        }

        let wrapped_body = format!("{{ {} }}", config.body);
        let _body_block: syn::Block = parse_str(&wrapped_body)
            .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid loop body syntax: {e}")))?;

        if config.kind != LoopKind::Loop {
            let cond_str = config.condition.as_deref().unwrap_or("").trim();
            if cond_str.is_empty() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "language.loop: condition is required for {:?}",
                    config.kind
                )));
            }
            if config.kind == LoopKind::While {
                let _cond: syn::Expr = parse_str(cond_str)
                    .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid while condition syntax: {e}")))?;
            } else if config.kind == LoopKind::For {
                let for_pattern_expr = format!("for {} in [].iter() {{}}", cond_str);
                let _for: syn::Expr = parse_str(&for_pattern_expr)
                    .map_err(|e| TemplateError::ConfigMismatch(format!("Invalid for loop condition expression (must be 'pattern in expr'): {e}")))?;
            }
        }

        let upstream_edge = ctx.graph.edges.iter()
            .find(|e| e.target == ctx.node.id && e.target_port == "input");

        let loop_code = match config.kind {
            LoopKind::Loop => format!("loop {{\n        {}\n    }}", config.body),
            LoopKind::While => format!("while {} {{\n        {}\n    }}", config.condition.as_deref().unwrap_or(""), config.body),
            LoopKind::For => format!("for {} {{\n        {}\n    }}", config.condition.as_deref().unwrap_or(""), config.body),
        };

        let source = if let Some(edge) = upstream_edge {
            let up_var = crate::codegen::dataflow::get_value_var_name(&edge.source, &edge.source_port, ctx.graph);
            format!(
                "pub fn {}({}: impl Clone) -> impl Clone {{\n    {}\n}}\n",
                config.name, up_var, loop_code
            )
        } else {
            format!(
                "pub fn {}() -> impl Clone {{\n    {}\n}}\n",
                config.name, loop_code
            )
        };

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("functions/{}.rs", to_snake_case(&config.name)),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---------------------------------------------------------------------------
// language.propagate (S15c)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguagePropagateConfig {
    /// snake_case Rust function identifier.
    pub(super) name: String,
}

pub struct LanguagePropagate {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguagePropagate {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.propagate").expect("language.propagate id is static and valid"),
            display: TemplateDisplay::new(
                "Propagate",
                "Language",
                "Applies the '?' operator to propagate errors (Result/Option) up the call stack.",
            ),
            inputs: vec![PortSpec::single("input", "any", "A Result or Option value to propagate.")],
            outputs: vec![PortSpec::single("output", "any", "The unwrapped success value of the Result/Option.")],
            schema: serde_json::to_value(schemars::schema_for!(LanguagePropagateConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguagePropagate {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: LanguagePropagateConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        if !is_valid_snake_case_ident(&config.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.propagate: name {:?} is not a snake_case Rust identifier",
                config.name
            )));
        }

        let upstream_edge = ctx.graph.edges.iter()
            .find(|e| e.target == ctx.node.id && e.target_port == "input");

        let source = if let Some(edge) = upstream_edge {
            let up_var = crate::codegen::dataflow::get_value_var_name(&edge.source, &edge.source_port, ctx.graph);
            format!(
                "pub fn {}({}: Result<impl Clone, crate::errors::AppError>) -> Result<impl Clone, crate::errors::AppError> {{\n    Ok({}?)\n}}\n",
                config.name, up_var, up_var
            )
        } else {
            format!(
                "pub fn {}() -> Result<&'static str, crate::errors::AppError> {{\n    Ok(\"unwired\")\n}}\n",
                config.name
            )
        };

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("functions/{}.rs", to_snake_case(&config.name)),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---------------------------------------------------------------------------
// language.await (S15c)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguageAwaitConfig {
    /// snake_case Rust function identifier.
    pub(super) name: String,
}

pub struct LanguageAwait {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguageAwait {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.await").expect("language.await id is static and valid"),
            display: TemplateDisplay::new(
                "Await",
                "Language",
                "Visual await modifier representing the .await keyword for future values.",
            ),
            inputs: vec![PortSpec::single("input", "any", "An asynchronous Future value.")],
            outputs: vec![PortSpec::single("output", "any", "The resolved value of the Future.")],
            schema: serde_json::to_value(schemars::schema_for!(LanguageAwaitConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguageAwait {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: LanguageAwaitConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        if !is_valid_snake_case_ident(&config.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.await: name {:?} is not a snake_case Rust identifier",
                config.name
            )));
        }

        let upstream_edge = ctx.graph.edges.iter()
            .find(|e| e.target == ctx.node.id && e.target_port == "input");

        let source = if let Some(edge) = upstream_edge {
            let up_var = crate::codegen::dataflow::get_value_var_name(&edge.source, &edge.source_port, ctx.graph);
            format!(
                "pub async fn {}({}: impl std::future::Future<Output = impl Clone>) -> impl Clone {{\n    {}.await\n}}\n",
                config.name, up_var, up_var
            )
        } else {
            format!(
                "pub async fn {}() -> &'static str {{\n    \"unwired\"\n}}\n",
                config.name
            )
        };

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("functions/{}.rs", to_snake_case(&config.name)),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---------------------------------------------------------------------------
// language.pointer (S15c)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, JsonSchema, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum PointerKind {
    ArcNew,
    ArcClone,
    BoxNew,
}

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct LanguagePointerConfig {
    /// snake_case Rust function identifier.
    pub(super) name: String,
    /// ArcNew, ArcClone, or BoxNew pointer type.
    pub(super) kind: PointerKind,
}

pub struct LanguagePointer {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl LanguagePointer {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("language.pointer").expect("language.pointer id is static and valid"),
            display: TemplateDisplay::new(
                "Pointer",
                "Language",
                "Visual pointer wrappers representing Arc::new, Arc::clone, or Box::new.",
            ),
            inputs: vec![PortSpec::single("input", "any", "The value to wrap or clone.")],
            outputs: vec![PortSpec::single("output", "any", "The wrapped pointer value.")],
            schema: serde_json::to_value(schemars::schema_for!(LanguagePointerConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for LanguagePointer {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: LanguagePointerConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        if !is_valid_snake_case_ident(&config.name) {
            return Err(TemplateError::ConfigMismatch(format!(
                "language.pointer: name {:?} is not a snake_case Rust identifier",
                config.name
            )));
        }

        let upstream_edge = ctx.graph.edges.iter()
            .find(|e| e.target == ctx.node.id && e.target_port == "input");

        let wrap_code = match config.kind {
            PointerKind::ArcNew => "std::sync::Arc::new(input)",
            PointerKind::ArcClone => "std::sync::Arc::clone(&input)",
            PointerKind::BoxNew => "std::boxed::Box::new(input)",
        };

        let source = if let Some(edge) = upstream_edge {
            let up_var = crate::codegen::dataflow::get_value_var_name(&edge.source, &edge.source_port, ctx.graph);
            let replaced_wrap = wrap_code.replace("input", &up_var);
            format!(
                "pub fn {}({}: impl Clone) -> impl Clone {{\n    {}\n}}\n",
                config.name, up_var, replaced_wrap
            )
        } else {
            format!(
                "pub fn {}() -> &'static str {{\n    \"unwired\"\n}}\n",
                config.name
            )
        };

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("functions/{}.rs", to_snake_case(&config.name)),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}
