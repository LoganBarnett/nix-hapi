//! Server-side JSON-RPC 2.0 host loop for provider binaries.
//!
//! Provider binaries call `run` with their `Provider` implementation.  This
//! function reads newline-delimited JSON-RPC requests from stdin, dispatches
//! each to the provider, and writes responses to stdout.  It returns when
//! stdin is closed.

use crate::field_value::ResolvedFieldValue;
use crate::meta::NixHapiMeta;
use crate::plan::ProviderPlan;
use crate::provider::{Provider, ResolvedConfig};
use serde_json::{json, Value};
use std::collections::HashMap;
use thiserror::Error;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Error)]
pub enum ProviderHostError {
  #[error("Failed to read from stdin: {0}")]
  Stdin(io::Error),

  #[error("Failed to write to stdout: {0}")]
  Stdout(io::Error),

  #[error("Failed to parse JSON-RPC request: {0}")]
  RequestParse(serde_json::Error),
}

/// Reads JSON-RPC requests from stdin, dispatches to `provider`, and writes
/// responses to stdout.  Returns when stdin is closed.
pub async fn run<P: Provider>(provider: P) -> Result<(), ProviderHostError> {
  let stdin = io::stdin();
  let stdout = io::stdout();
  let mut reader = BufReader::new(stdin);
  let mut out = stdout;

  let mut line = String::new();
  loop {
    line.clear();
    let bytes_read = reader
      .read_line(&mut line)
      .await
      .map_err(ProviderHostError::Stdin)?;
    if bytes_read == 0 {
      break;
    }
    if line.trim().is_empty() {
      continue;
    }

    let request: Value =
      serde_json::from_str(&line).map_err(ProviderHostError::RequestParse)?;

    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request["method"].as_str().unwrap_or("").to_string();
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    let response = dispatch(&provider, &method, params, id).await;
    let response_line = serde_json::to_string(&response)
      .expect("response serialization infallible");

    out
      .write_all(response_line.as_bytes())
      .await
      .map_err(ProviderHostError::Stdout)?;
    out
      .write_all(b"\n")
      .await
      .map_err(ProviderHostError::Stdout)?;
    out.flush().await.map_err(ProviderHostError::Stdout)?;
  }

  Ok(())
}

async fn dispatch<P: Provider>(
  provider: &P,
  method: &str,
  params: Value,
  id: Value,
) -> Value {
  match method {
    "list_live" => {
      let config = match parse_config(&params["config"]) {
        Ok(c) => c,
        Err(e) => return error_response(id, e),
      };
      match provider.list_live(&config, &[]).await {
        Ok(result) => success_response(id, result),
        Err(e) => error_response(id, e.to_string()),
      }
    }
    "plan" => {
      let config = match parse_config(&params["config"]) {
        Ok(c) => c,
        Err(e) => return error_response(id, e),
      };
      let meta: NixHapiMeta =
        match serde_json::from_value(params["meta"].clone()) {
          Ok(m) => m,
          Err(e) => return error_response(id, e.to_string()),
        };
      match provider
        .plan(&params["desired"], &params["live"], &meta, &config)
        .await
      {
        Ok(plan) => {
          let plan_value =
            serde_json::to_value(plan).expect("plan serialization infallible");
          success_response(id, plan_value)
        }
        Err(e) => error_response(id, e.to_string()),
      }
    }
    "apply" => {
      let config = match parse_config(&params["config"]) {
        Ok(c) => c,
        Err(e) => return error_response(id, e),
      };
      let plan: ProviderPlan =
        match serde_json::from_value(params["plan"].clone()) {
          Ok(p) => p,
          Err(e) => return error_response(id, e.to_string()),
        };
      match provider.apply(&plan, &config).await {
        Ok(report) => {
          let report_value = serde_json::to_value(report)
            .expect("report serialization infallible");
          success_response(id, report_value)
        }
        Err(e) => error_response(id, e.to_string()),
      }
    }
    other => error_response(id, format!("unknown method: {other}")),
  }
}

/// Deserializes the wire config format and reconstructs the correct
/// `ResolvedFieldValue` variant for each field.
///
/// The wire format carries `{"tag": "managed"|"initial", "value": "..."}` per
/// field.  For backwards compatibility, plain string values are treated as
/// `Managed`.
fn parse_config(config_value: &Value) -> Result<ResolvedConfig, String> {
  if config_value.is_null() {
    return Ok(HashMap::new());
  }
  let map: HashMap<String, Value> =
    serde_json::from_value(config_value.clone())
      .map_err(|e| format!("failed to parse config: {e}"))?;
  map
    .into_iter()
    .map(|(k, v)| {
      let rfv = match v {
        Value::Object(ref obj) => {
          let tag =
            obj.get("tag").and_then(|t| t.as_str()).unwrap_or("managed");
          let value = obj
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
              format!("config field {k:?}: missing \"value\" in wire object")
            })?
            .to_string();
          match tag {
            "initial" => ResolvedFieldValue::Initial(value),
            _ => ResolvedFieldValue::Managed(value),
          }
        }
        // Plain string → backwards compatibility.
        Value::String(s) => ResolvedFieldValue::Managed(s),
        _ => {
          return Err(format!(
            "config field {k:?}: expected object or string, got {v}"
          ))
        }
      };
      Ok((k, rfv))
    })
    .collect()
}

fn success_response(id: Value, result: Value) -> Value {
  json!({ "jsonrpc": "2.0", "result": result, "id": id })
}

fn error_response(id: Value, message: impl ToString) -> Value {
  json!({
    "jsonrpc": "2.0",
    "error": { "code": -32000, "message": message.to_string() },
    "id": id
  })
}
