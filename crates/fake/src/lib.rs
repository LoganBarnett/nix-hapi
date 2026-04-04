//! Fake provider for testing wave ordering and parallel execution.
//!
//! `FakeProvider` is a second `Provider` implementation (after the LDAP
//! provider) used exclusively in tests.  It always produces one synthetic
//! `Add` change so that `apply` is always invoked, sleeps for a configurable
//! duration, and records timing information to a shared list so callers can
//! assert on overlap and sequencing.

use async_trait::async_trait;
use nix_hapi_lib::meta::NixHapiMeta;
use nix_hapi_lib::plan::{
  ApplyReport, FieldDiff, ProviderPlan, ResourceChange, RunbookStep,
};
use nix_hapi_lib::provider::{Filter, Provider, ProviderError, ResolvedConfig};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Timing record produced by one `apply` call.
pub struct ApplyRecord {
  pub instance_name: String,
  pub started_at: Instant,
  pub finished_at: Instant,
}

impl ApplyRecord {
  /// Returns true if this record's execution interval overlaps with `other`.
  ///
  /// Two half-open intervals [s1, f1) and [s2, f2) overlap when s1 < f2 and
  /// s2 < f1.
  pub fn overlaps_with(&self, other: &ApplyRecord) -> bool {
    self.started_at < other.finished_at && other.started_at < self.finished_at
  }
}

/// A provider that sleeps and records timing.  Suitable only for tests.
pub struct FakeProvider {
  records: Arc<Mutex<Vec<ApplyRecord>>>,
  /// Value returned by `list_live`.  Defaults to an empty object.
  live_state: serde_json::Value,
  /// Accumulates a clone of every `desired` argument passed to `plan`.
  plan_snapshots: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl FakeProvider {
  pub fn new(records: Arc<Mutex<Vec<ApplyRecord>>>) -> Self {
    Self {
      records,
      live_state: serde_json::json!({}),
      plan_snapshots: Arc::new(Mutex::new(Vec::new())),
    }
  }

  /// Constructs a provider that returns `live_state` from `list_live`,
  /// simulating post-apply values such as API-assigned IDs.
  pub fn with_live_state(
    records: Arc<Mutex<Vec<ApplyRecord>>>,
    live_state: serde_json::Value,
  ) -> Self {
    Self {
      records,
      live_state,
      plan_snapshots: Arc::new(Mutex::new(Vec::new())),
    }
  }

  /// Replaces the plan-snapshot recorder with a caller-supplied one.
  ///
  /// The caller retains a clone of the arc so it can inspect the desired
  /// states that were passed to `plan` after the wave completes.
  pub fn with_plan_snapshots(
    mut self,
    snapshots: Arc<Mutex<Vec<serde_json::Value>>>,
  ) -> Self {
    self.plan_snapshots = snapshots;
    self
  }

  /// Returns a clone of the shared plan-snapshot recorder.
  pub fn plan_snapshots(&self) -> Arc<Mutex<Vec<serde_json::Value>>> {
    Arc::clone(&self.plan_snapshots)
  }
}

#[async_trait]
impl Provider for FakeProvider {
  fn provider_type(&self) -> &str {
    "fake"
  }

  fn sensitive_config_fields(&self) -> &[&str] {
    &[]
  }

  async fn list_live(
    &self,
    _config: &ResolvedConfig,
    _filters: &[Filter],
  ) -> Result<serde_json::Value, ProviderError> {
    Ok(self.live_state.clone())
  }

  async fn plan(
    &self,
    desired: &serde_json::Value,
    _live: &serde_json::Value,
    _meta: &NixHapiMeta,
    _config: &ResolvedConfig,
  ) -> Result<ProviderPlan, ProviderError> {
    self.plan_snapshots.lock().unwrap().push(desired.clone());
    Ok(ProviderPlan {
      instance_name: String::new(),
      provider_type: "fake".to_string(),
      changes: vec![ResourceChange::Add {
        resource_id: "fake-resource".to_string(),
        fields: vec![FieldDiff {
          field: "synthetic".to_string(),
          from: None,
          to: Some("true".to_string()),
        }],
      }],
      runbook: vec![RunbookStep {
        description: "Create fake resource".to_string(),
        command: "fake-add fake-resource".to_string(),
        body: None,
        operation: serde_json::json!({"action": "add", "id": "fake-resource"}),
      }],
    })
  }

  async fn apply(
    &self,
    plan: &ProviderPlan,
    config: &ResolvedConfig,
  ) -> Result<ApplyReport, ProviderError> {
    let delay_ms: u64 = config
      .get("delayMs")
      .and_then(|v| v.value())
      .and_then(|s| s.parse().ok())
      .unwrap_or(0);

    let started_at = Instant::now();
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    let finished_at = Instant::now();

    self.records.lock().unwrap().push(ApplyRecord {
      instance_name: plan.instance_name.clone(),
      started_at,
      finished_at,
    });

    Ok(ApplyReport {
      created: vec!["fake-resource".to_string()],
      modified: vec![],
      deleted: vec![],
    })
  }
}
