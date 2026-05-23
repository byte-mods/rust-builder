//! WebAssembly dynamically-loaded execution block template (S27 SDK).
//!
//! Emits wasmi-based host-execution runtime scaffolding to dynamically
//! load, instantiate, and invoke an exported function inside a `.wasm` file at runtime.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::templates::{
    ports::PortSpec,
    codegen::{CodegenCtx, EmittedItem, RuntimeEmission},
    DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateError, TemplateId,
};
use super::{id_or_panic, schema_value};

#[derive(Debug, JsonSchema, Deserialize, Serialize, Clone)]
pub struct WasmRunnerConfig {
    /// Relative path to the compiled WebAssembly (.wasm) file from project root.
    pub wasm_file: String,
    /// The name of the exported WebAssembly function to invoke.
    pub function_name: String,
}

pub struct WasmRunner {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl WasmRunner {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("wasm.runner"),
            display: TemplateDisplay::new(
                "Wasm Runner",
                "Integration",
                "Dynamically loads, instantiates, and executes an exported function in a secure WebAssembly (.wasm) plugin.",
            ),
            inputs: vec![PortSpec::single("input", "i32", "Numeric input parameter for the Wasm function.")],
            outputs: vec![PortSpec::single("output", "i32", "Numeric result returned by the Wasm function.")],
            schema: schema_value::<WasmRunnerConfig>(),
        }
    }
}

impl NodeTemplate for WasmRunner {
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
        let config: WasmRunnerConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let wasm_file = config.wasm_file.replace('\\', "/");
        let function_name = config.function_name.trim();

        if function_name.is_empty() {
            return Err(TemplateError::SchemaInvalid("function_name cannot be empty".to_string()));
        }

        // Generate the native, async function executing the Wasm runner
        let source_code = format!(
            r#"//! WebAssembly runner worker execution function.
//! Automatically compiled and managed by rust_no_code visual builder.

use std::fs::File;
use std::path::Path;
use wasmi::{{Engine, Module, Store, Linker}};

/// Securely loads and invokes `{function_name}` inside WebAssembly module `{wasm_file}` in-memory.
pub async fn execute(input: i32) -> Result<i32, anyhow::Error> {{
    let wasm_path = Path::new("{wasm_file}");
    if !wasm_path.exists() {{
        return Err(anyhow::anyhow!("WebAssembly plugin file not found at path: {{}}", wasm_path.display()));
    }}

    let mut file = File::open(wasm_path)?;
    let engine = Engine::default();
    let module = Module::new(&engine, &mut file)?;
    
    let mut store = Store::new(&engine, ());
    let linker = Linker::new(&engine);
    let instance = linker.instantiate(&mut store, &module)?.start(&mut store)?;
    
    let func = instance.get_typed_func::<i32, i32>(&store, "{function_name}")?;
    let result = func.call(&mut store, input)?;
    
    Ok(result)
}}
"#
        );

        let module_name = format!("wasm_runner_{}", ctx.node.id.0.replace('-', "_"));

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("src/functions/{}.rs", module_name),
                source: source_code,
            }],
            dependencies: vec![
                ("wasmi".to_string(), "0.31".to_string()),
            ],
            debug_site: None,
        })
    }
}
