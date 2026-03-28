//! Wave-parallel execution of provider instances.
//!
//! This module lifts the plan and apply loops out of the CLI binary so that
//! tests and alternative front-ends can drive reconciliation without going
//! through the binary.  The caller supplies a `resolve_provider` closure that
//! maps a provider type string to a boxed [`Provider`]; the executor handles
//! DAG-wave scheduling, config resolution, live-state fetching, and threading.

use crate::dag::{execution_waves, DagError};
use crate::meta::NixHapiMeta;
use crate::plan::{ApplyReport, Plan, ProviderPlan};
use crate::provider::{
  resolve_config, Provider, ProviderError, ResolvedConfig,
};
use serde_json::Value;
use thiserror::Error;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ExecuteError {
  #[error("Instance {instance:?} is missing a __nixhapi.provider declaration")]
  MissingProvider { instance: String },

  #[error(
    "Failed to resolve provider configuration for instance {instance:?}: {source}"
  )]
  ConfigResolution {
    instance: String,
    #[source]
    source: ProviderError,
  },

  #[error("Provider operation failed for instance {instance:?}: {source}")]
  ProviderOperation {
    instance: String,
    #[source]
    source: ProviderError,
  },

  #[error("Unknown provider type {provider_type:?} for instance {instance:?}")]
  ProviderLookup {
    instance: String,
    provider_type: String,
  },

  #[error(
    "Failed to initialize provider for instance {instance:?}: {message}"
  )]
  ProviderInit { instance: String, message: String },

  #[error("Failed to resolve provider dependency order: {0}")]
  DependencyResolution(#[from] DagError),
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Splits a top-level scope value into its `__nixhapi` metadata and the
/// remaining data subtree that the provider receives.
fn split_scope(scope: &Value) -> (NixHapiMeta, Value) {
  let meta: NixHapiMeta = scope
    .get("__nixhapi")
    .and_then(|v| serde_json::from_value(v.clone()).ok())
    .unwrap_or_default();

  let data = match scope.as_object() {
    Some(obj) => {
      let filtered: serde_json::Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "__nixhapi")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
      Value::Object(filtered)
    }
    None => Value::Null,
  };

  (meta, data)
}

/// Resolves config, calls `list_live`, and computes the plan for one instance.
///
/// Returns the plan together with the provider and resolved config so that
/// `execute_apply_waves` can call `apply` immediately without re-resolving.
fn plan_instance_inner<F>(
  instance_name: &str,
  root: &Value,
  resolve_fn: &F,
) -> Result<(ProviderPlan, Box<dyn Provider>, ResolvedConfig), ExecuteError>
where
  F: Fn(&str, &str) -> Result<Box<dyn Provider>, ExecuteError>,
{
  let scope_value = root.get(instance_name).expect("instance must exist");
  let (meta, data) = split_scope(scope_value);

  let spec =
    meta
      .provider
      .as_ref()
      .ok_or_else(|| ExecuteError::MissingProvider {
        instance: instance_name.to_string(),
      })?;

  let config = resolve_config(&spec.config).map_err(|source| {
    ExecuteError::ConfigResolution {
      instance: instance_name.to_string(),
      source,
    }
  })?;

  let provider = resolve_fn(instance_name, &spec.provider_type)?;

  let live = provider.list_live(&config, &[]).map_err(|source| {
    ExecuteError::ProviderOperation {
      instance: instance_name.to_string(),
      source,
    }
  })?;

  let mut pp =
    provider
      .plan(&data, &live, &meta, &config)
      .map_err(|source| ExecuteError::ProviderOperation {
        instance: instance_name.to_string(),
        source,
      })?;

  pp.instance_name = instance_name.to_string();
  Ok((pp, provider, config))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Plans all provider instances across DAG waves, running each wave in
/// parallel.
///
/// Provider plans in the returned [`Plan`] are stored in topological order
/// (wave 0 first), preserving correct sequencing for runbook display.
pub fn execute_plan_waves<F>(
  root: &Value,
  resolve_provider: F,
) -> Result<Plan, ExecuteError>
where
  F: Fn(&str, &str) -> Result<Box<dyn Provider>, ExecuteError> + Sync,
{
  let mut plan = Plan::default();
  let waves = execution_waves(root)?;

  for wave in &waves {
    let wave_results: Vec<Result<ProviderPlan, ExecuteError>> =
      std::thread::scope(|scope| {
        wave
          .iter()
          .map(|name| {
            scope.spawn(|| {
              plan_instance_inner(name.as_str(), root, &resolve_provider)
                .map(|(pp, _, _)| pp)
            })
          })
          .collect::<Vec<_>>()
          .into_iter()
          .map(|h| h.join().expect("provider planning thread panicked"))
          .collect()
      });
    for result in wave_results {
      plan.provider_plans.push(result?);
    }
  }

  Ok(plan)
}

/// Plans and applies all provider instances across DAG waves, running each
/// wave in parallel.
///
/// All instances in wave N complete before any instance in wave N+1 begins.
/// Instances with no changes skip the apply call and return an empty report.
pub fn execute_apply_waves<F>(
  root: &Value,
  resolve_provider: F,
) -> Result<Vec<(String, ApplyReport)>, ExecuteError>
where
  F: Fn(&str, &str) -> Result<Box<dyn Provider>, ExecuteError> + Sync,
{
  let waves = execution_waves(root)?;
  let mut reports: Vec<(String, ApplyReport)> = Vec::new();

  for wave in &waves {
    let wave_results: Vec<Result<(String, ApplyReport), ExecuteError>> =
      std::thread::scope(|scope| {
        wave
          .iter()
          .map(|name| {
            scope.spawn(|| {
              let (pp, provider, config) =
                plan_instance_inner(name.as_str(), root, &resolve_provider)?;
              if pp.is_empty() {
                return Ok((name.to_string(), ApplyReport::default()));
              }
              provider
                .apply(&pp, &config)
                .map_err(|source| ExecuteError::ProviderOperation {
                  instance: name.to_string(),
                  source,
                })
                .map(|report| (name.to_string(), report))
            })
          })
          .collect::<Vec<_>>()
          .into_iter()
          .map(|h| h.join().expect("provider apply thread panicked"))
          .collect()
      });
    for result in wave_results {
      reports.push(result?);
    }
  }

  Ok(reports)
}
