//! Client-side subprocess provider: spawns a provider binary and communicates
//! with it via JSON-RPC 2.0 over stdin/stdout.

use crate::meta::NixHapiMeta;
use crate::plan::{ApplyReport, ProviderPlan};
use crate::provider::{Filter, Provider, ProviderError, ResolvedConfig};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SubprocessError {
  #[error("Failed to spawn provider process at {path:?}: {source}")]
  Spawn {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },

  #[error("Failed to communicate with provider process: {source}")]
  Communication {
    #[source]
    source: std::io::Error,
  },

  #[error("Failed to parse provider response: {0}")]
  ResponseParse(serde_json::Error),

  #[error("Provider returned error: {0}")]
  ProviderError(String),
}

struct SubprocessInner {
  child: Child,
  stdin: BufWriter<ChildStdin>,
  stdout: BufReader<ChildStdout>,
}

pub struct SubprocessProvider {
  type_name: String,
  inner: Mutex<SubprocessInner>,
}

impl SubprocessProvider {
  pub fn spawn(
    type_name: String,
    path: &Path,
  ) -> Result<Self, SubprocessError> {
    let mut child = Command::new(path)
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .spawn()
      .map_err(|source| SubprocessError::Spawn {
        path: path.to_owned(),
        source,
      })?;

    let stdin = BufWriter::new(child.stdin.take().expect("stdin piped"));
    let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    Ok(Self {
      type_name,
      inner: Mutex::new(SubprocessInner {
        child,
        stdin,
        stdout,
      }),
    })
  }

  fn call(
    &self,
    method: &str,
    params: Value,
  ) -> Result<Value, SubprocessError> {
    let mut inner = self.inner.lock().expect("inner mutex poisoned");

    let request = json!({
      "jsonrpc": "2.0",
      "method": method,
      "params": params,
      "id": 1
    });

    let request_line = serde_json::to_string(&request)
      .expect("request serialization infallible");

    writeln!(inner.stdin, "{}", request_line)
      .map_err(|source| SubprocessError::Communication { source })?;
    inner
      .stdin
      .flush()
      .map_err(|source| SubprocessError::Communication { source })?;

    let mut response_line = String::new();
    inner
      .stdout
      .read_line(&mut response_line)
      .map_err(|source| SubprocessError::Communication { source })?;

    let response: Value = serde_json::from_str(response_line.trim_end())
      .map_err(SubprocessError::ResponseParse)?;

    if let Some(error) = response.get("error") {
      let message = error
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown provider error")
        .to_string();
      return Err(SubprocessError::ProviderError(message));
    }

    Ok(response["result"].clone())
  }
}

impl Drop for SubprocessProvider {
  fn drop(&mut self) {
    if let Ok(inner) = self.inner.get_mut() {
      // Kill rather than wait for graceful EOF so Drop is not blocking.
      let _ = inner.child.kill();
      let _ = inner.child.wait();
    }
  }
}

/// Converts a `ResolvedConfig` to a plain string map for the wire protocol,
/// omitting `Unmanaged` fields.
fn to_wire_config(config: &ResolvedConfig) -> HashMap<String, String> {
  config
    .iter()
    .filter_map(|(k, v)| v.value().map(|s| (k.clone(), s.to_string())))
    .collect()
}

impl Provider for SubprocessProvider {
  fn provider_type(&self) -> &str {
    &self.type_name
  }

  fn sensitive_config_fields(&self) -> &[&str] {
    // Runbook scrubbing is performed by the provider binary itself; the
    // subprocess wrapper has no visibility into which fields are sensitive.
    &[]
  }

  fn list_live(
    &self,
    config: &ResolvedConfig,
    _filters: &[Filter],
  ) -> Result<Value, ProviderError> {
    self
      .call(
        "list_live",
        json!({ "config": to_wire_config(config), "filters": [] }),
      )
      .map_err(|e| ProviderError::OperationFailed(e.to_string()))
  }

  fn plan(
    &self,
    desired: &Value,
    live: &Value,
    meta: &NixHapiMeta,
    config: &ResolvedConfig,
  ) -> Result<ProviderPlan, ProviderError> {
    let result = self
      .call(
        "plan",
        json!({
          "desired": desired,
          "live": live,
          "meta": meta,
          "config": to_wire_config(config),
        }),
      )
      .map_err(|e| ProviderError::OperationFailed(e.to_string()))?;

    serde_json::from_value(result)
      .map_err(|e| ProviderError::DesiredStateParse(e.to_string()))
  }

  fn apply(
    &self,
    plan: &ProviderPlan,
    config: &ResolvedConfig,
  ) -> Result<ApplyReport, ProviderError> {
    let result = self
      .call("apply", json!({ "plan": plan, "config": to_wire_config(config) }))
      .map_err(|e| ProviderError::OperationFailed(e.to_string()))?;

    serde_json::from_value(result)
      .map_err(|e| ProviderError::DesiredStateParse(e.to_string()))
  }
}
