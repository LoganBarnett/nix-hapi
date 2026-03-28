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
use std::io::{self, BufRead, Write};
use thiserror::Error;

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
pub fn run<P: Provider>(provider: P) -> Result<(), ProviderHostError> {
  let stdin = io::stdin();
  let stdout = io::stdout();
  let mut out = stdout.lock();

  for line in stdin.lock().lines() {
    let line = line.map_err(ProviderHostError::Stdin)?;
    if line.trim().is_empty() {
      continue;
    }

    let request: Value =
      serde_json::from_str(&line).map_err(ProviderHostError::RequestParse)?;

    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request["method"].as_str().unwrap_or("").to_string();
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    let response = dispatch(&provider, &method, params, id);
    let response_line = serde_json::to_string(&response)
      .expect("response serialization infallible");

    writeln!(out, "{}", response_line).map_err(ProviderHostError::Stdout)?;
    out.flush().map_err(ProviderHostError::Stdout)?;
  }

  Ok(())
}

fn dispatch<P: Provider>(
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
      match provider.list_live(&config, &[]) {
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
      match provider.plan(&params["desired"], &params["live"], &meta, &config) {
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
      match provider.apply(&plan, &config) {
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

/// Deserializes a `HashMap<String,String>` from the wire and reconstructs it
/// as a `ResolvedConfig` with every value treated as `Managed`.
fn parse_config(config_value: &Value) -> Result<ResolvedConfig, String> {
  if config_value.is_null() {
    return Ok(HashMap::new());
  }
  let map: HashMap<String, String> =
    serde_json::from_value(config_value.clone())
      .map_err(|e| format!("failed to parse config: {e}"))?;
  Ok(
    map
      .into_iter()
      .map(|(k, v)| (k, ResolvedFieldValue::Managed(v)))
      .collect(),
  )
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
