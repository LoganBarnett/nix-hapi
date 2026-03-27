use nix_hapi_fake::{ApplyRecord, FakeProvider};
use nix_hapi_lib::dag::execution_waves;
use nix_hapi_lib::executor::{execute_apply_waves, ExecuteError};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ── Test fixture ──────────────────────────────────────────────────────────────

/// Builds a diamond dependency graph with four instances:
///
/// ```text
/// wave 0:  a
///          │
/// wave 1:  b   c   (both depend on a)
///           \ /
/// wave 2:    d     (depends on b and c)
/// ```
///
/// Each instance uses the fake provider and will sleep for `delay_ms`
/// milliseconds during apply.
fn diamond_dag_json(delay_ms: u64) -> HashMap<String, Value> {
  let scope = |depends_on: &[&str]| {
    serde_json::json!({
      "__nixhapi": {
        "provider": {
          "type": "fake",
          "delayMs": {"__nixhapi": "managed", "value": delay_ms.to_string()}
        },
        "dependsOn": depends_on
      }
    })
  };

  let mut map = HashMap::new();
  map.insert("a".to_string(), scope(&[]));
  map.insert("b".to_string(), scope(&[".a"]));
  map.insert("c".to_string(), scope(&[".a"]));
  map.insert("d".to_string(), scope(&[".b", ".c"]));
  map
}

fn make_resolver(
  records: &Arc<Mutex<Vec<ApplyRecord>>>,
) -> impl Fn(
  &str,
  &str,
) -> Result<Box<dyn nix_hapi_lib::provider::Provider>, ExecuteError>
     + Sync
     + '_ {
  move |instance: &str, provider_type: &str| match provider_type {
    "fake" => Ok(Box::new(FakeProvider::new(records.clone()))
      as Box<dyn nix_hapi_lib::provider::Provider>),
    other => Err(ExecuteError::ProviderLookup {
      instance: instance.to_string(),
      provider_type: other.to_string(),
    }),
  }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn wave_structure_is_correct() {
  let top_level = diamond_dag_json(0);
  let waves = execution_waves(&top_level).unwrap();
  assert_eq!(
    waves,
    vec![
      vec!["a".to_string()],
      vec!["b".to_string(), "c".to_string()],
      vec!["d".to_string()],
    ]
  );
}

#[test]
fn all_providers_apply() {
  let records: Arc<Mutex<Vec<ApplyRecord>>> = Arc::new(Mutex::new(Vec::new()));
  let top_level = diamond_dag_json(0);
  execute_apply_waves(&top_level, make_resolver(&records)).unwrap();

  let locked = records.lock().unwrap();
  let names: std::collections::HashSet<&str> =
    locked.iter().map(|r| r.instance_name.as_str()).collect();
  assert!(names.contains("a"), "a must have applied");
  assert!(names.contains("b"), "b must have applied");
  assert!(names.contains("c"), "c must have applied");
  assert!(names.contains("d"), "d must have applied");
}

#[test]
fn wave1_runs_in_parallel() {
  let records: Arc<Mutex<Vec<ApplyRecord>>> = Arc::new(Mutex::new(Vec::new()));
  // 50 ms each; if sequential they would not overlap.
  let top_level = diamond_dag_json(50);
  execute_apply_waves(&top_level, make_resolver(&records)).unwrap();

  let locked = records.lock().unwrap();
  let b = locked
    .iter()
    .find(|r| r.instance_name == "b")
    .expect("b must exist");
  let c = locked
    .iter()
    .find(|r| r.instance_name == "c")
    .expect("c must exist");
  assert!(
    b.overlaps_with(c),
    "b and c must overlap (ran in parallel within wave 1)"
  );
}

#[test]
fn wave2_waits_for_wave1() {
  let records: Arc<Mutex<Vec<ApplyRecord>>> = Arc::new(Mutex::new(Vec::new()));
  let top_level = diamond_dag_json(20);
  execute_apply_waves(&top_level, make_resolver(&records)).unwrap();

  let locked = records.lock().unwrap();
  let b = locked
    .iter()
    .find(|r| r.instance_name == "b")
    .expect("b must exist");
  let c = locked
    .iter()
    .find(|r| r.instance_name == "c")
    .expect("c must exist");
  let d = locked
    .iter()
    .find(|r| r.instance_name == "d")
    .expect("d must exist");
  assert!(d.started_at >= b.finished_at, "d must not start before b finishes");
  assert!(d.started_at >= c.finished_at, "d must not start before c finishes");
}
