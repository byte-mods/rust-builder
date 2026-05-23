//! Custom programmatic block node template (S16/S17 SDK).
//!
//! Emits arbitrary Rust function code inside the visual canvas dataflow.
//! Backend statically parses the signature using `syn` to bind ports.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use syn::{parse_str, ItemFn, FnArg, Pat, ReturnType};
use quote::ToTokens;

use crate::templates::{
    ports::PortSpec,
    codegen::{CodegenCtx, EmittedItem, RuntimeEmission},
    DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateError, TemplateId,
};
use super::{id_or_panic, schema_value, to_snake_case};

#[derive(Debug, JsonSchema, Deserialize, Serialize, Clone)]
pub struct CustomPort {
    pub name: String,
    pub ty: String,
}

#[derive(Debug, JsonSchema, Deserialize, Serialize, Clone)]
pub struct CustomBlockConfig {
    /// Snake_case function/module name for the custom block.
    pub name: String,
    /// The raw Rust function code.
    pub code: String,
    /// Dynamic inputs (populated by backend syn parsing).
    #[serde(default)]
    pub inputs: Vec<CustomPort>,
    /// Dynamic outputs (populated by backend syn parsing).
    #[serde(default)]
    pub outputs: Vec<CustomPort>,
}

pub struct CustomBlock {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl CustomBlock {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("custom.block"),
            display: TemplateDisplay::new(
                "Custom Block",
                "Custom",
                "Arbitrary Rust function code. Statically parsed and bound to visual canvas ports.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: schema_value::<CustomBlockConfig>(),
        }
    }
}

/// Static signature parser using the `syn` crate.
///
/// Parses a Rust raw function, extracting its parameter names/types as inputs,
/// and its output return type as output.
pub fn parse_signature(code: &str) -> Result<(Vec<CustomPort>, Vec<CustomPort>), String> {
    let item_fn: ItemFn = parse_str(code)
        .map_err(|e| format!("failed to parse function: {e}"))?;

    let mut inputs = Vec::new();
    for arg in item_fn.sig.inputs {
        match arg {
            FnArg::Receiver(_) => {
                return Err("receiver (self) parameters are not allowed in custom visual blocks".to_string());
            }
            FnArg::Typed(pat_type) => {
                let name = match &*pat_type.pat {
                    Pat::Ident(pat_ident) => pat_ident.ident.to_string(),
                    _ => return Err("pattern parameter bindings are not allowed".to_string()),
                };
                let ty = pat_type.ty.to_token_stream().to_string();
                inputs.push(CustomPort { name, ty });
            }
        }
    }

    let mut outputs = Vec::new();
    match item_fn.sig.output {
        ReturnType::Default => {
            outputs.push(CustomPort {
                name: "output".to_string(),
                ty: "()".to_string(),
            });
        }
        ReturnType::Type(_, ty) => {
            outputs.push(CustomPort {
                name: "output".to_string(),
                ty: ty.to_token_stream().to_string(),
            });
        }
    }

    Ok((inputs, outputs))
}

impl NodeTemplate for CustomBlock {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::Default }

    fn emit_runtime(
        &self,
        ctx: &CodegenCtx<'_>,
    ) -> Result<RuntimeEmission, TemplateError> {
        let config: CustomBlockConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        // Guard against panics
        if config.code.contains(".unwrap()") || config.code.contains(".expect(") || config.code.contains("panic!") {
            return Err(TemplateError::SchemaInvalid(
                "unwrap(), expect(), and panic! are prohibited inside custom blocks. Use ? propagation instead.".to_string()
            ));
        }

        // Just emit the function code directly into the functions directory
        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("functions/{}.rs", to_snake_case(&config.name)),
                source: config.code,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_signature_standard() {
        let code = r#"
            pub fn add(x: i32, y: i32) -> i32 {
                x + y
            }
        "#;
        let (inputs, outputs) = parse_signature(code).unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].name, "x");
        assert_eq!(inputs[0].ty, "i32");
        assert_eq!(inputs[1].name, "y");
        assert_eq!(inputs[1].ty, "i32");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "output");
        assert_eq!(outputs[0].ty, "i32");
    }

    #[test]
    fn test_parse_signature_default_return() {
        let code = r#"
            fn log_msg(msg: String) {
                println!("{}", msg);
            }
        "#;
        let (inputs, outputs) = parse_signature(code).unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "msg");
        assert_eq!(inputs[0].ty, "String");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "output");
        assert_eq!(outputs[0].ty, "()");
    }

    #[test]
    fn test_parse_signature_receiver_rejected() {
        let code = r#"
            fn save(&self, val: u32) {}
        "#;
        let err = parse_signature(code).unwrap_err();
        assert!(err.contains("receiver (self) parameters are not allowed"));
    }

    #[test]
    fn test_parse_signature_pattern_rejected() {
        let code = r#"
            fn unpack((a, b): (u32, u32)) {}
        "#;
        let err = parse_signature(code).unwrap_err();
        assert!(err.contains("pattern parameter bindings are not allowed"));
    }

    #[test]
    fn test_parse_signature_invalid_syntax() {
        let code = r#"
            fn hello(x: i32 {
        "#;
        let err = parse_signature(code).unwrap_err();
        assert!(err.contains("failed to parse function"));
    }
}
