//! Wave-parallel execution of provider instances.
//!
//! This module lifts the plan and apply loops out of the CLI binary so that
//! tests and alternative front-ends can drive reconciliation without going
//! through the binary.  The caller supplies a `resolve_provider` closure that
//! maps a provider type string to a boxed [`Provider`]; the executor handles
//! DAG-wave scheduling, config resolution, live-state fetching, and async
//! task parallelism.

use crate::dag::{execution_waves, DagError};
use crate::derived::{resolve_derived_from_tree, DerivedFromError};
use crate::meta::NixHapiMeta;
use crate::plan::{ApplyReport, Plan, ProviderPlan};
use crate::provider::{
  resolve_config, Provider, ProviderError, ResolvedConfig,
};
use crate::saturation::{check_derived_from_saturation, SaturationError};
use serde_json::Value;
use std::sync::Arc;
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

  #[error("Failed to resolve derivedFrom fields between waves: {0}")]
  DerivedFromResolution(#[from] DerivedFromError),

  #[error("Malformed __nixhapi metadata in instance {instance:?}: {message}")]
  MetadataParse { instance: String, message: String },

  #[error("Instance {instance:?} not found in root")]
  MissingInstance { instance: String },

  #[error("DerivedFrom saturation check failed: {0}")]
  SaturationCheck(#[from] SaturationError),

  #[error(
    "Partial wave failure: {errors:?} (successful reports still collected)"
  )]
  PartialWaveFailure {
    errors: Vec<ExecuteError>,
    successful_reports: Vec<(String, ApplyReport)>,
  },
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Splits a top-level scope value into its `__nixhapi` metadata and the
/// remaining data subtree that the provider receives.
fn split_scope(
  instance_name: &str,
  scope: &Value,
) -> Result<(NixHapiMeta, Value), ExecuteError> {
  let meta: NixHapiMeta = match scope.get("__nixhapi") {
    Some(v) if v.is_object() => {
      serde_json::from_value(v.clone()).map_err(|e| {
        ExecuteError::MetadataParse {
          instance: instance_name.to_string(),
          message: e.to_string(),
        }
      })?
    }
    _ => NixHapiMeta::default(),
  };

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

  Ok((meta, data))
}

/// Resolves config, calls `list_live`, and computes the plan for one instance.
///
/// Returns the plan together with the provider and resolved config so that
/// `execute_apply_waves` can call `apply` immediately without re-resolving.
async fn plan_instance_inner<F>(
  instance_name: &str,
  root: &Value,
  resolve_fn: &F,
) -> Result<(ProviderPlan, Box<dyn Provider>, ResolvedConfig), ExecuteError>
where
  F: Fn(&str, &str) -> Result<Box<dyn Provider>, ExecuteError>,
{
  let scope_value =
    root
      .get(instance_name)
      .ok_or_else(|| ExecuteError::MissingInstance {
        instance: instance_name.to_string(),
      })?;
  let (meta, data) = split_scope(instance_name, scope_value)?;

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

  let live = provider.list_live(&config, &[]).await.map_err(|source| {
    ExecuteError::ProviderOperation {
      instance: instance_name.to_string(),
      source,
    }
  })?;

  let mut pp =
    provider
      .plan(&data, &live, &meta, &config)
      .await
      .map_err(|source| ExecuteError::ProviderOperation {
        instance: instance_name.to_string(),
        source,
      })?;

  pp.instance_name = instance_name.to_string();
  Ok((pp, provider, config))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Plans all provider instances across DAG waves, running each wave in
/// parallel via tokio tasks.
///
/// Provider plans in the returned [`Plan`] are stored in topological order
/// (wave 0 first), preserving correct sequencing for runbook display.
pub async fn execute_plan_waves<F>(
  root: &Value,
  resolve_provider: F,
) -> Result<Plan, ExecuteError>
where
  F: Fn(&str, &str) -> Result<Box<dyn Provider>, ExecuteError>
    + Send
    + Sync
    + 'static,
{
  let mut plan = Plan::default();
  let waves = execution_waves(root)?;
  check_derived_from_saturation(root, &waves)?;

  let resolve_provider = Arc::new(resolve_provider);

  for wave in &waves {
    let mut handles = Vec::new();
    for name in wave {
      let name = name.clone();
      let root_clone = root.clone();
      let resolve_fn = Arc::clone(&resolve_provider);
      handles.push(tokio::spawn(async move {
        plan_instance_inner(&name, &root_clone, &*resolve_fn)
          .await
          .map(|(pp, _, _)| pp)
      }));
    }
    for handle in handles {
      let result = handle.await.expect("provider planning task panicked");
      plan.provider_plans.push(result?);
    }
  }

  Ok(plan)
}

/// Plans and applies all provider instances across DAG waves, running each
/// wave in parallel via tokio tasks.
///
/// All instances in wave N complete before any instance in wave N+1 begins.
/// Between waves, any `DerivedFrom` fields whose inputs are now available in
/// the crystalized live-state tree are resolved before the next wave plans.
pub async fn execute_apply_waves<F>(
  root: &Value,
  resolve_provider: F,
) -> Result<Vec<(String, ApplyReport)>, ExecuteError>
where
  F: Fn(&str, &str) -> Result<Box<dyn Provider>, ExecuteError>
    + Send
    + Sync
    + 'static,
{
  let waves = execution_waves(root)?;
  check_derived_from_saturation(root, &waves)?;
  let mut reports: Vec<(String, ApplyReport)> = Vec::new();
  // Crystalized tree: post-apply live state keyed by instance name.
  let mut crystalized = serde_json::json!({});
  // Active desired-state tree, updated between waves as DerivedFrom fields resolve.
  let mut active_root = root.clone();

  let resolve_provider = Arc::new(resolve_provider);

  for wave in &waves {
    active_root = resolve_derived_from_tree(&active_root, &crystalized)?;

    let mut handles = Vec::new();
    for name in wave {
      let name = name.clone();
      let root_clone = active_root.clone();
      let resolve_fn = Arc::clone(&resolve_provider);
      handles.push(tokio::spawn(async move {
        let (pp, provider, config) =
          plan_instance_inner(&name, &root_clone, &*resolve_fn).await?;
        if pp.is_empty() {
          return Ok((name, ApplyReport::default(), provider, config));
        }
        provider
          .apply(&pp, &config)
          .await
          .map_err(|source| ExecuteError::ProviderOperation {
            instance: name.clone(),
            source,
          })
          .map(|report| (name, report, provider, config))
      }));
    }

    let mut wave_errors: Vec<ExecuteError> = Vec::new();
    let mut wave_successes: Vec<(String, ApplyReport)> = Vec::new();
    for handle in handles {
      let result = handle.await.expect("provider apply task panicked");
      match result {
        Ok((name, report, provider, config)) => {
          // Capture post-apply live state so subsequent waves can resolve
          // their DerivedFrom inputs.
          match provider.list_live(&config, &[]).await {
            Ok(live) => {
              crystalized[&name] = live;
              wave_successes.push((name, report));
            }
            Err(source) => {
              wave_errors.push(ExecuteError::ProviderOperation {
                instance: name,
                source,
              });
            }
          }
        }
        Err(e) => wave_errors.push(e),
      }
    }
    // Always preserve successful reports, even when other instances failed.
    reports.extend(wave_successes.iter().map(|(n, r)| {
      (
        n.clone(),
        ApplyReport {
          created: r.created.clone(),
          modified: r.modified.clone(),
          deleted: r.deleted.clone(),
        },
      )
    }));
    if !wave_errors.is_empty() {
      return Err(ExecuteError::PartialWaveFailure {
        errors: wave_errors,
        successful_reports: wave_successes,
      });
    }
  }

  Ok(reports)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::meta::NixHapiMeta;
  use crate::plan::{
    ApplyReport, FieldDiff, ProviderPlan, ResourceChange, RunbookStep,
  };
  use crate::provider::{Filter, Provider, ProviderError, ResolvedConfig};
  use async_trait::async_trait;
  use serde_json::json;
  use std::collections::HashMap;
  use std::sync::{Arc, Mutex};

  /// A minimal in-process test provider.
  struct TestProvider {
    live_state: Value,
    should_error_on_plan: bool,
    should_error_on_apply: bool,
    apply_called: Arc<Mutex<Vec<String>>>,
  }

  impl TestProvider {
    fn ok(live_state: Value) -> Self {
      Self {
        live_state,
        should_error_on_plan: false,
        should_error_on_apply: false,
        apply_called: Arc::new(Mutex::new(Vec::new())),
      }
    }

    fn failing_plan() -> Self {
      Self {
        live_state: json!({}),
        should_error_on_plan: true,
        should_error_on_apply: false,
        apply_called: Arc::new(Mutex::new(Vec::new())),
      }
    }

    fn failing_apply(apply_called: Arc<Mutex<Vec<String>>>) -> Self {
      Self {
        live_state: json!({}),
        should_error_on_plan: false,
        should_error_on_apply: true,
        apply_called,
      }
    }
  }

  #[async_trait]
  impl Provider for TestProvider {
    fn provider_type(&self) -> &str {
      "test"
    }

    fn sensitive_config_fields(&self) -> &[&str] {
      &[]
    }

    async fn list_live(
      &self,
      _config: &ResolvedConfig,
      _filters: &[Filter],
    ) -> Result<Value, ProviderError> {
      Ok(self.live_state.clone())
    }

    async fn plan(
      &self,
      _desired: &Value,
      _live: &Value,
      _meta: &NixHapiMeta,
      _config: &ResolvedConfig,
    ) -> Result<ProviderPlan, ProviderError> {
      if self.should_error_on_plan {
        return Err(ProviderError::OperationFailed(
          "planned failure".to_string(),
        ));
      }
      Ok(ProviderPlan {
        instance_name: String::new(),
        provider_type: "test".to_string(),
        changes: vec![ResourceChange::Add {
          resource_id: "r".to_string(),
          fields: vec![FieldDiff {
            field: "f".to_string(),
            from: None,
            to: Some("v".to_string()),
          }],
        }],
        runbook: vec![RunbookStep {
          description: "add r".to_string(),
          command: "add r".to_string(),
          body: None,
          operation: json!({"action": "add"}),
        }],
      })
    }

    async fn apply(
      &self,
      plan: &ProviderPlan,
      _config: &ResolvedConfig,
    ) -> Result<ApplyReport, ProviderError> {
      self
        .apply_called
        .lock()
        .unwrap()
        .push(plan.instance_name.clone());
      if self.should_error_on_apply {
        return Err(ProviderError::OperationFailed(
          "apply failure".to_string(),
        ));
      }
      Ok(ApplyReport {
        created: vec!["r".to_string()],
        modified: vec![],
        deleted: vec![],
      })
    }
  }

  fn scope() -> Value {
    json!({
      "__nixhapi": {
        "provider": {"type": "test"},
        "dependsOn": []
      }
    })
  }

  fn scope_with_dep(dep: &str) -> Value {
    json!({
      "__nixhapi": {
        "provider": {"type": "test"},
        "dependsOn": [dep]
      }
    })
  }

  fn make_resolver(
    providers: HashMap<String, TestProvider>,
  ) -> impl Fn(&str, &str) -> Result<Box<dyn Provider>, ExecuteError>
       + Send
       + Sync
       + 'static {
    let providers = Arc::new(Mutex::new(providers));
    move |instance_name: &str, _type_name: &str| {
      let map = providers.lock().unwrap();
      if map.contains_key(instance_name) {
        // Cannot move out of the map, so construct a fresh provider with
        // the same configuration.
        let p = &map[instance_name];
        Ok(Box::new(TestProvider {
          live_state: p.live_state.clone(),
          should_error_on_plan: p.should_error_on_plan,
          should_error_on_apply: p.should_error_on_apply,
          apply_called: Arc::clone(&p.apply_called),
        }) as Box<dyn Provider>)
      } else {
        Err(ExecuteError::ProviderLookup {
          instance: instance_name.to_string(),
          provider_type: _type_name.to_string(),
        })
      }
    }
  }

  #[tokio::test]
  async fn plan_single_instance_produces_changes() {
    let root = json!({"alpha": scope()});
    let providers: HashMap<String, TestProvider> =
      [("alpha".to_string(), TestProvider::ok(json!({})))]
        .into_iter()
        .collect();
    let plan = execute_plan_waves(&root, make_resolver(providers))
      .await
      .unwrap();
    assert_eq!(plan.provider_plans.len(), 1);
    assert_eq!(plan.provider_plans[0].instance_name, "alpha");
    assert!(!plan.provider_plans[0].is_empty());
  }

  #[tokio::test]
  async fn apply_single_instance_returns_report() {
    let root = json!({"alpha": scope()});
    let apply_called = Arc::new(Mutex::new(Vec::new()));
    let providers: HashMap<String, TestProvider> = [(
      "alpha".to_string(),
      TestProvider {
        live_state: json!({}),
        should_error_on_plan: false,
        should_error_on_apply: false,
        apply_called: Arc::clone(&apply_called),
      },
    )]
    .into_iter()
    .collect();
    let reports = execute_apply_waves(&root, make_resolver(providers))
      .await
      .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].0, "alpha");
    assert_eq!(reports[0].1.created, vec!["r".to_string()]);
    let called = apply_called.lock().unwrap();
    assert_eq!(*called, vec!["alpha"]);
  }

  #[tokio::test]
  async fn diamond_dag_respects_wave_ordering() {
    // a → {b, c} → d
    let root = json!({
      "a": scope(),
      "b": scope_with_dep(r#".["a"]"#),
      "c": scope_with_dep(r#".["a"]"#),
      "d": json!({
        "__nixhapi": {
          "provider": {"type": "test"},
          "dependsOn": [r#".["b"]"#, r#".["c"]"#]
        }
      })
    });
    let apply_called = Arc::new(Mutex::new(Vec::new()));
    let providers: HashMap<String, TestProvider> = ["a", "b", "c", "d"]
      .into_iter()
      .map(|name| {
        (
          name.to_string(),
          TestProvider {
            live_state: json!({}),
            should_error_on_plan: false,
            should_error_on_apply: false,
            apply_called: Arc::clone(&apply_called),
          },
        )
      })
      .collect();
    let reports = execute_apply_waves(&root, make_resolver(providers))
      .await
      .unwrap();
    assert_eq!(reports.len(), 4);

    let called = apply_called.lock().unwrap();
    let pos = |name: &str| called.iter().position(|s| s == name).unwrap();
    // a must come before b, c, and d.
    assert!(pos("a") < pos("b"));
    assert!(pos("a") < pos("c"));
    assert!(pos("b") < pos("d"));
    assert!(pos("c") < pos("d"));
  }

  #[tokio::test]
  async fn missing_provider_declaration_returns_error() {
    // Scope without __nixhapi.provider.
    let root = json!({
      "alpha": { "__nixhapi": {} }
    });
    let providers: HashMap<String, TestProvider> =
      [("alpha".to_string(), TestProvider::ok(json!({})))]
        .into_iter()
        .collect();
    let err = execute_plan_waves(&root, make_resolver(providers))
      .await
      .unwrap_err();
    assert!(
      matches!(err, ExecuteError::MissingProvider { .. }),
      "expected MissingProvider, got {err:?}"
    );
  }

  #[tokio::test]
  async fn provider_plan_error_propagated() {
    let root = json!({"alpha": scope()});
    let providers: HashMap<String, TestProvider> =
      [("alpha".to_string(), TestProvider::failing_plan())]
        .into_iter()
        .collect();
    let err = execute_plan_waves(&root, make_resolver(providers))
      .await
      .unwrap_err();
    assert!(
      matches!(err, ExecuteError::ProviderOperation { .. }),
      "expected ProviderOperation, got {err:?}"
    );
  }

  #[tokio::test]
  async fn provider_apply_error_propagated() {
    let root = json!({"alpha": scope()});
    let apply_called = Arc::new(Mutex::new(Vec::new()));
    let providers: HashMap<String, TestProvider> =
      [("alpha".to_string(), TestProvider::failing_apply(apply_called))]
        .into_iter()
        .collect();
    let err = execute_apply_waves(&root, make_resolver(providers))
      .await
      .unwrap_err();
    assert!(
      matches!(err, ExecuteError::PartialWaveFailure { .. }),
      "expected PartialWaveFailure, got {err:?}"
    );
  }

  #[tokio::test]
  async fn malformed_metadata_returns_error() {
    // __nixhapi has an invalid shape: provider should be an object, not a
    // number.  The DAG walker encounters this first during execution_waves.
    let root = json!({
      "alpha": {
        "__nixhapi": {"provider": 42}
      }
    });
    let providers: HashMap<String, TestProvider> =
      [("alpha".to_string(), TestProvider::ok(json!({})))]
        .into_iter()
        .collect();
    let err = execute_plan_waves(&root, make_resolver(providers))
      .await
      .unwrap_err();
    assert!(
      matches!(
        err,
        ExecuteError::DependencyResolution(
          crate::dag::DagError::MetadataParse { .. }
        )
      ),
      "expected DependencyResolution(MetadataParse), got {err:?}"
    );
  }
}
