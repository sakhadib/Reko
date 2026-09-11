use serde::{Deserialize, Serialize};

// Top-level IR
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IrFunction {
    pub ir_version: String,
    pub identity: Identity,
    pub source: Source,
    pub declaration: Declaration,
    pub signature: Signature,
    pub context: Context,
    pub body: Body,
    pub variables: Variables,
    pub calls: Vec<Call>,
    pub dependencies: Dependencies,
    pub data_flow: DataFlow,
    pub control_flow_graph: ControlFlowGraph,
    pub types: Types,
    pub behavior: Behavior,
    pub complexity: Complexity,
    pub nested: Nested,
    pub relationships: Relationships,
    pub tests: Tests,
    pub build: Build,
    pub execution: Execution,
    pub security: Security,
    pub language_specific: serde_json::Value,
    pub metadata: Metadata,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub language: String,
    pub language_version: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Source {
    pub file: String,
    pub module: String,
    pub source_text: String,
    pub normalized_source: String,
    pub hash: String,
    pub encoding: String,
    pub location: Location,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Location {
    pub start: Position,
    pub end: Position,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Position {
    pub line: usize,
    pub column: usize,
    pub byte: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Declaration {
    pub visibility: String,
    pub modifiers: Vec<String>,
    pub attributes: Vec<String>,
    pub annotations: Vec<String>,
    pub documentation: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Signature {
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub type_parameters: Vec<String>,
    pub constraints: Vec<String>,
    pub throws: Vec<String>,
    pub effects: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "type")]
    pub typ: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Context {
    pub namespace: Option<String>,
    pub module: Option<String>,
    pub package: Option<String>,
    pub class: Option<String>,
    pub struct_: Option<String>,
    pub interface: Option<String>,
    #[serde(rename = "trait")]
    pub trait_: Option<String>,
    pub parent_function: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Body {
    pub raw: String,
    pub ast: Ast,
    pub statements: Vec<String>,
    pub expressions: Vec<String>,
    pub control_flow: ControlFlow,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Ast {
    #[serde(rename = "type")]
    pub typ: String,
    pub children: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ControlFlow {
    pub branches: Vec<String>,
    pub loops: Vec<String>,
    pub switches: Vec<String>,
    pub exception_handlers: Vec<String>,
    pub early_returns: Vec<String>,
    pub breaks: Vec<String>,
    pub continues: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Variables {
    pub parameters: Vec<String>,
    pub locals: Vec<String>,
    pub globals_read: Vec<String>,
    pub globals_written: Vec<String>,
    pub captures: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Call {
    pub target: String,
    pub qualified_target: String,
    pub arguments: Vec<String>,
    pub location: serde_json::Value,
    pub dynamic: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Dependencies {
    pub functions: Vec<String>,
    pub types: Vec<String>,
    pub constants: Vec<String>,
    pub variables: Vec<String>,
    pub modules: Vec<String>,
    pub imports: Vec<String>,
    pub external_symbols: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DataFlow {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub assignments: Vec<String>,
    pub definitions: Vec<String>,
    pub uses: Vec<String>,
    pub dependencies: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ControlFlowGraph {
    pub entry: String,
    pub exit: String,
    pub nodes: Vec<String>,
    pub edges: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Types {
    pub inferred: serde_json::Value,
    pub explicit: serde_json::Value,
    pub generic: Vec<String>,
    pub aliases: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Behavior {
    pub pure: Option<bool>,
    pub deterministic: Option<bool>,
    pub side_effects: Vec<String>,
    pub mutates_arguments: Vec<String>,
    pub mutates_global_state: Option<bool>,
    pub reads_external_state: Option<bool>,
    pub writes_external_state: Option<bool>,
    pub io: Vec<String>,
    pub exceptions: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Complexity {
    pub cyclomatic: Option<usize>,
    pub cognitive: Option<usize>,
    pub lines: Option<usize>,
    pub branches: Option<usize>,
    pub loops: Option<usize>,
    pub nesting_depth: Option<usize>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Nested {
    pub functions: Vec<String>,
    pub closures: Vec<String>,
    pub lambdas: Vec<String>,
    pub local_types: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Relationships {
    pub calls: Vec<String>,
    pub called_by: Vec<String>,
    pub overrides: Vec<String>,
    pub overridden_by: Vec<String>,
    pub implements: Vec<String>,
    pub implemented_by: Vec<String>,
    pub references: Vec<String>,
    pub referenced_by: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Tests {
    pub tests: Vec<String>,
    pub coverage: Option<String>,
    pub assertions: Vec<String>,
    pub fixtures: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Build {
    pub compile_time_dependencies: Vec<String>,
    pub runtime_dependencies: Vec<String>,
    pub feature_flags: Vec<String>,
    pub conditional_compilation: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Execution {
    #[serde(rename = "async")]
    pub is_async: bool,
    pub concurrent: bool,
    pub generator: bool,
    pub coroutine: bool,
    pub recursive: bool,
    pub reentrant: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Security {
    pub trust_boundary: Option<String>,
    pub input_sources: Vec<String>,
    pub sensitive_operations: Vec<String>,
    pub security_annotations: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Metadata {
    pub parser: Option<String>,
    pub parser_version: Option<String>,
    pub confidence: f64,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub custom: serde_json::Value,
}

impl IrFunction {
    pub fn new_minimal(
        id: String,
        name: String,
        qualified_name: String,
        file: String,
        module: String,
        source_text: String,
        hash: String,
        start: Position,
        end: Position,
        visibility: String,
        modifiers: Vec<String>,
        return_type: Option<String>,
        parameters: Vec<Parameter>,
        throws: Vec<String>,
        package: Option<String>,
        class: Option<String>,
    ) -> Self {
        let lines = source_text.lines().count();
        let body_raw = source_text.clone();
        IrFunction {
            ir_version: "1.0".to_string(),
            identity: Identity {
                id: id.clone(),
                name,
                qualified_name: qualified_name.clone(),
                kind: "function".to_string(),
                language: "java".to_string(),
                language_version: None,
            },
            source: Source {
                file: file.clone(),
                module,
                source_text: source_text.clone(),
                normalized_source: source_text.trim().to_string(),
                hash,
                encoding: "utf-8".to_string(),
                location: Location { start, end },
            },
            declaration: Declaration {
                visibility,
                modifiers,
                attributes: vec![],
                annotations: vec![],
                documentation: None,
            },
            signature: Signature {
                parameters,
                return_type,
                type_parameters: vec![],
                constraints: vec![],
                throws,
                effects: vec![],
            },
            context: Context {
                namespace: package.clone(),
                module: package.clone(),
                package,
                class,
                struct_: None,
                interface: None,
                trait_: None,
                parent_function: None,
            },
            body: Body {
                raw: body_raw,
                ast: Ast {
                    typ: "block".to_string(),
                    children: vec![],
                },
                statements: vec![],
                expressions: vec![],
                control_flow: ControlFlow {
                    branches: vec![],
                    loops: vec![],
                    switches: vec![],
                    exception_handlers: vec![],
                    early_returns: vec![],
                    breaks: vec![],
                    continues: vec![],
                },
            },
            variables: Variables {
                parameters: vec![],
                locals: vec![],
                globals_read: vec![],
                globals_written: vec![],
                captures: vec![],
            },
            calls: vec![],
            dependencies: Dependencies {
                functions: vec![],
                types: vec![],
                constants: vec![],
                variables: vec![],
                modules: vec![],
                imports: vec![],
                external_symbols: vec![],
            },
            data_flow: DataFlow {
                inputs: vec![],
                outputs: vec![],
                assignments: vec![],
                definitions: vec![],
                uses: vec![],
                dependencies: vec![],
            },
            control_flow_graph: ControlFlowGraph {
                entry: "node_0".to_string(),
                exit: "node_n".to_string(),
                nodes: vec![],
                edges: vec![],
            },
            types: Types {
                inferred: serde_json::json!({}),
                explicit: serde_json::json!({}),
                generic: vec![],
                aliases: vec![],
            },
            behavior: Behavior {
                pure: None,
                deterministic: None,
                side_effects: vec![],
                mutates_arguments: vec![],
                mutates_global_state: None,
                reads_external_state: None,
                writes_external_state: None,
                io: vec![],
                exceptions: vec![],
            },
            complexity: Complexity {
                cyclomatic: None,
                cognitive: None,
                lines: Some(lines),
                branches: None,
                loops: None,
                nesting_depth: None,
            },
            nested: Nested {
                functions: vec![],
                closures: vec![],
                lambdas: vec![],
                local_types: vec![],
            },
            relationships: Relationships {
                calls: vec![],
                called_by: vec![],
                overrides: vec![],
                overridden_by: vec![],
                implements: vec![],
                implemented_by: vec![],
                references: vec![],
                referenced_by: vec![],
            },
            tests: Tests {
                tests: vec![],
                coverage: None,
                assertions: vec![],
                fixtures: vec![],
            },
            build: Build {
                compile_time_dependencies: vec![],
                runtime_dependencies: vec![],
                feature_flags: vec![],
                conditional_compilation: vec![],
            },
            execution: Execution {
                is_async: false,
                concurrent: false,
                generator: false,
                coroutine: false,
                recursive: false,
                reentrant: None,
            },
            security: Security {
                trust_boundary: None,
                input_sources: vec![],
                sensitive_operations: vec![],
                security_annotations: vec![],
            },
            language_specific: serde_json::json!({}),
            metadata: Metadata {
                parser: Some("reko-javaExtractor".to_string()),
                parser_version: Some("0.1.0".to_string()),
                confidence: 1.0,
                warnings: vec![],
                errors: vec![],
                custom: serde_json::json!({}),
            },
        }
    }
}
