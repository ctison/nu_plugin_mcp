use nu_plugin::{EngineInterface, EvaluatedCall, SimplePluginCommand};
use nu_protocol::{
    Category, Example, LabeledError, Record, Signature, Span, SyntaxShape, Type, Value,
};

use crate::descriptor::parse_tools;
use crate::plugin::McpPlugin;

/// Constructs records that the root `mcp` command accepts as tool descriptors.
pub struct McpToolCommand;

impl SimplePluginCommand for McpToolCommand {
    type Plugin = McpPlugin;

    /// Returns the Nushell subcommand name.
    fn name(&self) -> &'static str {
        "mcp tool"
    }

    /// Defines the descriptor-constructor arguments and output type.
    fn signature(&self) -> Signature {
        Signature::build(self.name())
            .required("name", SyntaxShape::String, "MCP tool name")
            .required(
                "closure",
                SyntaxShape::Closure(None),
                "Nushell closure invoked with one arguments record",
            )
            .named(
                "title",
                SyntaxShape::String,
                "human-readable tool title",
                None,
            )
            .named(
                "description",
                SyntaxShape::String,
                "human-readable tool description",
                None,
            )
            .named(
                "input-schema",
                SyntaxShape::Record(vec![].into()),
                "JSON Schema for the arguments record",
                None,
            )
            .named(
                "output-schema",
                SyntaxShape::Record(vec![].into()),
                "optional JSON Schema for structured output",
                None,
            )
            .named(
                "annotations",
                SyntaxShape::Record(vec![].into()),
                "MCP tool annotation hints",
                None,
            )
            .input_output_type(
                Type::Nothing,
                Type::Record(nu_protocol::CollectionColumns::default()),
            )
            .category(Category::Experimental)
    }

    /// Describes the command for Nushell help and completions.
    fn description(&self) -> &'static str {
        "Construct an MCP tool descriptor from a Nushell closure"
    }

    /// Shows a minimal descriptor with an explicit argument schema.
    fn examples(&self) -> Vec<Example<'_>> {
        vec![Example {
            example: r#"mcp tool greet {|args| $"Hello, ($args.name)!"} --description "Greet someone" --input-schema {type: object, properties: {name: {type: string}}, required: [name]}"#,
            description: "Create a greeting tool descriptor",
            result: None,
        }]
    }

    /// Builds a record while preserving the closure value and source spans.
    fn run(
        &self,
        _plugin: &Self::Plugin,
        _engine: &EngineInterface,
        call: &EvaluatedCall,
        _input: &Value,
    ) -> Result<Value, LabeledError> {
        let span = call.head;
        let name: String = call.req(0)?;
        let closure = call.nth(1).ok_or_else(|| missing_closure_error(span))?;

        if !matches!(closure, Value::Closure { .. }) {
            return Err(missing_closure_error(closure.span()));
        }

        let mut descriptor = Record::new();
        descriptor.push("name", Value::string(name, span));
        descriptor.push("closure", closure);
        copy_optional_flag(call, &mut descriptor, "title", "title");
        copy_optional_flag(call, &mut descriptor, "description", "description");
        copy_optional_flag(call, &mut descriptor, "input-schema", "input_schema");
        copy_optional_flag(call, &mut descriptor, "output-schema", "output_schema");
        copy_optional_flag(call, &mut descriptor, "annotations", "annotations");

        let descriptor = Value::record(descriptor, span);
        parse_tools(&Value::list(vec![descriptor.clone()], span))?;
        Ok(descriptor)
    }
}

/// Copies an optional named argument into the descriptor without losing its type or span.
fn copy_optional_flag(call: &EvaluatedCall, record: &mut Record, flag: &str, field: &str) {
    if let Some(value) = call.get_flag_value(flag) {
        record.push(field, value);
    }
}

/// Creates a labeled error for a missing or invalid closure argument.
fn missing_closure_error(span: Span) -> LabeledError {
    LabeledError::new("Expected a Nushell closure")
        .with_label("the tool handler must be a closure", span)
}
