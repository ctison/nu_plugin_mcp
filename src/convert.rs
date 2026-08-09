use nu_protocol::{Record, Span, Value};
use serde_json::{Map, Number};

/// Converts an MCP JSON value into the closest Nushell value.
pub fn json_to_nu(value: &serde_json::Value, span: Span) -> Result<Value, String> {
  match value {
    serde_json::Value::Null => Ok(Value::nothing(span)),
    serde_json::Value::Bool(value) => Ok(Value::bool(*value, span)),
    serde_json::Value::Number(value) => json_number_to_nu(value, span),
    serde_json::Value::String(value) => Ok(Value::string(value, span)),
    serde_json::Value::Array(values) => values
      .iter()
      .map(|value| json_to_nu(value, span))
      .collect::<Result<Vec<_>, _>>()
      .map(|values| Value::list(values, span)),
    serde_json::Value::Object(values) => {
      let mut record = Record::with_capacity(values.len());
      for (key, value) in values {
        record.push(key, json_to_nu(value, span)?);
      }
      Ok(Value::record(record, span))
    },
  }
}

/// Converts a Nushell value into its JSON-compatible representation.
pub fn nu_to_json(value: &Value) -> Result<serde_json::Value, String> {
  match value {
    Value::Nothing { .. } => Ok(serde_json::Value::Null),
    Value::Bool { val, .. } => Ok(serde_json::Value::Bool(*val)),
    Value::Int { val, .. } | Value::Duration { val, .. } => {
      Ok(serde_json::Value::Number((*val).into()))
    },
    Value::Float { val, .. } => Number::from_f64(*val)
      .map(serde_json::Value::Number)
      .ok_or_else(|| "non-finite floats cannot be represented as JSON".into()),
    Value::String { val, .. } | Value::Glob { val, .. } => {
      Ok(serde_json::Value::String(val.clone()))
    },
    Value::Filesize { val, .. } => Ok(serde_json::Value::Number(val.get().into())),
    Value::Date { val, .. } => Ok(serde_json::Value::String(val.to_rfc3339())),
    Value::Binary { val, .. } => Ok(serde_json::Value::Array(
      val
        .iter()
        .map(|byte| serde_json::Value::Number(u64::from(*byte).into()))
        .collect(),
    )),
    Value::List { vals, .. } => vals
      .iter()
      .map(nu_to_json)
      .collect::<Result<Vec<_>, _>>()
      .map(serde_json::Value::Array),
    Value::Record { val, .. } => record_to_json(val),
    Value::Error { error, .. } => Err(error.to_string()),
    Value::Range { .. } => Err("ranges must be collected before returning them".into()),
    Value::Closure { .. } => Err("closures cannot be returned as MCP content".into()),
    Value::CellPath { .. } => Err("cell paths cannot be returned as MCP content".into()),
    Value::Custom { .. } => Err("custom values cannot be returned as MCP content".into()),
  }
}

/// Converts a Nushell record into a JSON object.
pub fn nu_record_to_json(value: &Value) -> Result<Map<String, serde_json::Value>, String> {
  match nu_to_json(value)? {
    serde_json::Value::Object(object) => Ok(object),
    _ => Err("expected a record".into()),
  }
}

/// Converts a JSON number while rejecting unsigned integers outside Nushell's range.
fn json_number_to_nu(value: &Number, span: Span) -> Result<Value, String> {
  if let Some(value) = value.as_i64() {
    return Ok(Value::int(value, span));
  }
  if let Some(value) = value.as_u64() {
    return i64::try_from(value)
      .map(|value| Value::int(value, span))
      .map_err(|_| "JSON integer exceeds Nushell's signed 64-bit range".into());
  }
  value
    .as_f64()
    .map(|value| Value::float(value, span))
    .ok_or_else(|| "invalid JSON number".into())
}

/// Converts every field in a Nushell record into a JSON object.
fn record_to_json(record: &Record) -> Result<serde_json::Value, String> {
  let mut object = Map::new();
  for (key, value) in record {
    object.insert(key.clone(), nu_to_json(value)?);
  }
  Ok(serde_json::Value::Object(object))
}

#[cfg(test)]
mod tests {
  use std::ops::Bound;

  use nu_protocol::{Record, Span, Value};
  use serde_json::json;

  use super::{json_to_nu, nu_to_json};

  /// Verifies that nested JSON records round-trip through Nushell values.
  #[test]
  fn round_trips_json_records() {
    let source = json!({"name": "Ada", "flags": [true, false], "count": 3});
    let nu = json_to_nu(&source, Span::test_data()).expect("JSON should convert");
    assert_eq!(nu_to_json(&nu).expect("Nu should convert"), source);
  }

  /// Verifies the special Nushell scalar encodings used by `to json`.
  #[test]
  fn converts_nushell_scalars() {
    let span = Span::test_data();
    let mut record = Record::new();
    record.push("duration", Value::duration(2_000_000_000, span));
    record.push("filesize", Value::filesize(2_000, span));
    record.push("binary", Value::binary(vec![222, 173], span));
    let value = Value::record(record, span);

    assert_eq!(
      nu_to_json(&value).expect("record should convert"),
      json!({"duration": 2_000_000_000_i64, "filesize": 2_000, "binary": [222, 173]})
    );
  }

  /// Rejects behavioral Nushell values instead of serializing misleading debug text.
  #[test]
  fn rejects_ranges() {
    let value = Value::test_range(nu_protocol::Range::new_int(
      Some(1),
      None,
      Some(Bound::Included(3)),
    ));
    assert!(nu_to_json(&value).is_err());
  }
}
