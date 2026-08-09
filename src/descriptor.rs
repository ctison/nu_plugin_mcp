use std::{collections::HashSet, sync::Arc};

use jsonschema::Validator;
use nu_protocol::{LabeledError, Span, Spanned, Value, engine::Closure};
use rust_mcp_sdk::schema::Tool;
use serde_json::{Map, json};

use crate::convert::{nu_record_to_json, nu_to_json};

const DESCRIPTOR_FIELDS: &[&str] = &[
  "name",
  "closure",
  "title",
  "description",
  "input_schema",
  "output_schema",
  "annotations",
];

const ANNOTATION_FIELDS: &[&str] = &[
  "title",
  "read_only_hint",
  "destructive_hint",
  "idempotent_hint",
  "open_world_hint",
];

/// A validated MCP definition paired with the Nushell closure that implements it.
pub struct NuTool {
  pub tool: Tool,
  pub closure: Spanned<Closure>,
  pub input_validator: Arc<Validator>,
  pub output_validator: Option<Arc<Validator>>,
}

/// Parses and validates the complete list of descriptors before a server starts.
pub fn parse_tools(value: &Value) -> Result<Vec<NuTool>, LabeledError> {
  let Value::List {
    vals: descriptors, ..
  } = value
  else {
    return Err(labeled(
      "Expected a list of MCP tool descriptors",
      "provide records created by `mcp tool`",
      value.span(),
    ));
  };

  let mut names = HashSet::with_capacity(descriptors.len());
  let mut tools = Vec::with_capacity(descriptors.len());
  for descriptor in descriptors {
    let tool = parse_tool(descriptor)
      .map_err(|message| labeled("Invalid MCP tool descriptor", message, descriptor.span()))?;
    if !names.insert(tool.tool.name.clone()) {
      return Err(labeled(
        "Duplicate MCP tool name",
        format!("tool `{}` is defined more than once", tool.tool.name),
        descriptor.span(),
      ));
    }
    tools.push(tool);
  }

  if tools.is_empty() {
    tracing::warn!("starting an MCP server with no tools");
  }
  Ok(tools)
}

/// Parses one record into its MCP metadata, schema validators, and closure.
fn parse_tool(value: &Value) -> Result<NuTool, String> {
  let record = value
    .as_record()
    .map_err(|_| "each tool descriptor must be a record".to_string())?;
  reject_unknown_fields(record, DESCRIPTOR_FIELDS, "descriptor")?;

  let name = required_string(record.get("name"), "name")?;
  validate_tool_name(&name)?;
  let closure_value = record
    .get("closure")
    .ok_or_else(|| "missing required `closure` field".to_string())?;
  let closure = closure_value
    .as_closure()
    .map_err(|_| "`closure` must contain a Nushell closure".to_string())?
    .clone();
  let closure = Spanned {
    item: closure,
    span: closure_value.span(),
  };

  let input_schema = optional_schema(record.get("input_schema"), "input_schema")?
    .unwrap_or_else(|| json!({"type": "object", "additionalProperties": true}));
  let output_schema = optional_schema(record.get("output_schema"), "output_schema")?;
  let input_validator = Arc::new(compile_schema(&input_schema, "input_schema")?);
  let output_validator = output_schema
    .as_ref()
    .map(|schema| compile_schema(schema, "output_schema").map(Arc::new))
    .transpose()?;

  let mut tool_json = Map::new();
  tool_json.insert("name".into(), serde_json::Value::String(name));
  tool_json.insert("inputSchema".into(), input_schema);
  copy_optional_string(record, &mut tool_json, "title", "title")?;
  copy_optional_string(record, &mut tool_json, "description", "description")?;
  if let Some(schema) = output_schema {
    tool_json.insert("outputSchema".into(), schema);
  }
  if let Some(annotations) = record.get("annotations") {
    tool_json.insert("annotations".into(), parse_annotations(annotations)?);
  }

  let tool = serde_json::from_value(serde_json::Value::Object(tool_json))
    .map_err(|error| format!("MCP metadata is invalid: {error}"))?;
  Ok(NuTool {
    tool,
    closure,
    input_validator,
    output_validator,
  })
}

/// Rejects misspelled or unsupported record fields with a focused message.
fn reject_unknown_fields(
  record: &nu_protocol::Record,
  allowed: &[&str],
  context: &str,
) -> Result<(), String> {
  for (field, _) in record {
    if !allowed.contains(&field.as_str()) {
      return Err(format!("unknown {context} field `{field}`"));
    }
  }
  Ok(())
}

/// Extracts a required string field.
fn required_string(value: Option<&Value>, field: &str) -> Result<String, String> {
  value
    .ok_or_else(|| format!("missing required `{field}` field"))?
    .as_str()
    .map(ToOwned::to_owned)
    .map_err(|_| format!("`{field}` must be a string"))
}

/// Copies an optional Nushell string into an MCP JSON object.
fn copy_optional_string(
  record: &nu_protocol::Record,
  target: &mut Map<String, serde_json::Value>,
  source: &str,
  destination: &str,
) -> Result<(), String> {
  if let Some(value) = record.get(source) {
    target.insert(
      destination.into(),
      serde_json::Value::String(
        value
          .as_str()
          .map_err(|_| format!("`{source}` must be a string"))?
          .to_owned(),
      ),
    );
  }
  Ok(())
}

/// Converts and checks an optional MCP schema record.
fn optional_schema(
  value: Option<&Value>,
  field: &str,
) -> Result<Option<serde_json::Value>, String> {
  value
    .map(|value| {
      let schema = nu_to_json(value).map_err(|error| format!("`{field}`: {error}"))?;
      ensure_object_schema(&schema, field)?;
      Ok(schema)
    })
    .transpose()
}

/// Enforces MCP's object-root restriction for tool schemas.
fn ensure_object_schema(schema: &serde_json::Value, field: &str) -> Result<(), String> {
  if schema
    .as_object()
    .and_then(|object| object.get("type"))
    .and_then(serde_json::Value::as_str)
    == Some("object")
  {
    Ok(())
  } else {
    Err(format!("`{field}` must be a record with `type: object`"))
  }
}

/// Compiles a schema without enabling network or filesystem reference resolution.
fn compile_schema(schema: &serde_json::Value, field: &str) -> Result<Validator, String> {
  jsonschema::draft202012::new(schema)
    .map_err(|error| format!("`{field}` is not valid JSON Schema 2020-12: {error}"))
}

/// Converts snake-case Nushell annotation fields into the MCP wire representation.
fn parse_annotations(value: &Value) -> Result<serde_json::Value, String> {
  let record = value
    .as_record()
    .map_err(|_| "`annotations` must be a record".to_string())?;
  reject_unknown_fields(record, ANNOTATION_FIELDS, "annotation")?;
  let source = nu_record_to_json(value)?;
  let mut target = Map::new();
  for (field, value) in source {
    let destination = match field.as_str() {
      "title" => "title",
      "read_only_hint" => "readOnlyHint",
      "destructive_hint" => "destructiveHint",
      "idempotent_hint" => "idempotentHint",
      "open_world_hint" => "openWorldHint",
      _ => unreachable!("unknown fields were rejected"),
    };
    if destination == "title" && !value.is_string() {
      return Err("annotation `title` must be a string".into());
    }
    if destination != "title" && !value.is_boolean() {
      return Err(format!("annotation `{field}` must be a boolean"));
    }
    target.insert(destination.into(), value);
  }
  Ok(serde_json::Value::Object(target))
}

/// Validates the MCP base-name grammar without adding a regular-expression dependency.
fn validate_tool_name(name: &str) -> Result<(), String> {
  let mut characters = name.chars();
  let first = characters
    .next()
    .ok_or_else(|| "tool name cannot be empty".to_string())?;
  if !first.is_ascii_alphanumeric() {
    return Err("tool name must begin with an ASCII letter or digit".into());
  }
  if !characters
    .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
  {
    return Err(
      "tool name may contain only ASCII letters, digits, hyphens, underscores, and dots".into(),
    );
  }
  if !name
    .chars()
    .next_back()
    .is_some_and(|character| character.is_ascii_alphanumeric())
  {
    return Err("tool name must end with an ASCII letter or digit".into());
  }
  Ok(())
}

/// Creates a consistently labeled Nushell-facing validation error.
fn labeled(title: impl Into<String>, label: impl Into<String>, span: Span) -> LabeledError {
  LabeledError::new(title).with_label(label, span)
}

#[cfg(test)]
mod tests {
  use nu_protocol::{Record, Span, Value};

  use super::{ensure_object_schema, parse_annotations, validate_tool_name};

  /// Accepts the MCP base-name punctuation in interior positions.
  #[test]
  fn validates_tool_names() {
    assert!(validate_tool_name("weather.current-v2").is_ok());
    assert!(validate_tool_name("-weather").is_err());
    assert!(validate_tool_name("weather-").is_err());
    assert!(validate_tool_name("weather now").is_err());
  }

  /// Requires schemas to advertise an object at their root.
  #[test]
  fn validates_object_schema_root() {
    assert!(ensure_object_schema(&serde_json::json!({"type": "object"}), "schema").is_ok());
    assert!(ensure_object_schema(&serde_json::json!({"type": "string"}), "schema").is_err());
  }

  /// Converts annotation names while enforcing their value types.
  #[test]
  fn parses_annotations_record() {
    let span = Span::test_data();
    let mut annotations = Record::new();
    annotations.push("read_only_hint", Value::bool(true, span));
    annotations.push("open_world_hint", Value::bool(false, span));
    let parsed =
      parse_annotations(&Value::record(annotations, span)).expect("annotations should parse");
    assert_eq!(
      parsed,
      serde_json::json!({"readOnlyHint": true, "openWorldHint": false})
    );
  }
}
