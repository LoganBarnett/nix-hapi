use nix_hapi_fake::{ApplyRecord, FakeProvider};
use nix_hapi_lib::executor::{execute_apply_waves, ExecuteError};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

// ── Test fixtures ─────────────────────────────────────────────────────────────

/// Resolver that creates FakeProviders with per-instance live states and
/// optional plan-snapshot recorders.
fn make_resolver<'a>(
  records: &'a Arc<Mutex<Vec<ApplyRecord>>>,
  live_states: std::collections::HashMap<&'static str, Value>,
  snapshot_map: std::collections::HashMap<&'static str, Arc<Mutex<Vec<Value>>>>,
) -> impl Fn(
  &str,
  &str,
) -> Result<Box<dyn nix_hapi_lib::provider::Provider>, ExecuteError>
     + Sync
     + 'a {
  move |instance: &str, provider_type: &str| match provider_type {
    "fake" => {
      let live = live_states
        .get(instance)
        .cloned()
        .unwrap_or_else(|| json!({}));
      let mut p = FakeProvider::with_live_state(Arc::clone(records), live);
      if let Some(snap) = snapshot_map.get(instance) {
        p = p.with_plan_snapshots(Arc::clone(snap));
      }
      Ok(Box::new(p) as Box<dyn nix_hapi_lib::provider::Provider>)
    }
    other => Err(ExecuteError::ProviderLookup {
      instance: instance.to_string(),
      provider_type: other.to_string(),
    }),
  }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Single-hop derivedFrom: instance `b` reads a value assigned by instance
/// `a`'s API (returned via `list_live` after apply).
///
/// ```text
/// wave 0: a  (list_live returns {"user_id": "uid-42"})
/// wave 1: b  (DerivedFrom reads .["a"].user_id → resolved to "uid-42")
/// ```
#[test]
fn single_hop_derived_from_resolved_before_wave1() {
  let records: Arc<Mutex<Vec<ApplyRecord>>> = Arc::new(Mutex::new(Vec::new()));
  let b_snapshots: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));

  let live_states =
    std::collections::HashMap::from([("a", json!({"user_id": "uid-42"}))]);
  let snapshot_map =
    std::collections::HashMap::from([("b", Arc::clone(&b_snapshots))]);

  let root = json!({
    "a": {
      "__nixhapi": {
        "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}}
      }
    },
    "b": {
      "__nixhapi": {
        "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}},
        "dependsOn": [r#".["a"]"#]
      },
      "userId": {
        "__nixhapi": "derived-from",
        "inputs": {"uid": r#".["a"].user_id"#},
        "expression": "mkManaged(.uid)"
      }
    }
  });

  execute_apply_waves(
    &root,
    make_resolver(&records, live_states, snapshot_map),
  )
  .expect("execute_apply_waves should succeed");

  let snaps = b_snapshots.lock().unwrap();
  assert_eq!(snaps.len(), 1, "b's plan should have been called once");
  let desired = &snaps[0];
  // The DerivedFrom should have been resolved to a managed FieldValue.
  assert_eq!(
    desired["userId"]["__nixhapi"],
    json!("managed"),
    "DerivedFrom must be resolved before b's plan is called"
  );
  assert_eq!(
    desired["userId"]["value"],
    json!("uid-42"),
    "Resolved value must come from a's live state"
  );
}

/// Multi-hop chain: a → b → c, each hop reading the previous instance's
/// API-assigned value.
///
/// ```text
/// wave 0: a  (live: {"id": "a-1"})
/// wave 1: b  (DerivedFrom reads a's id; live: {"id": "b-2"})
/// wave 2: c  (DerivedFrom reads b's id)
/// ```
#[test]
fn chained_derived_from_resolved_across_waves() {
  let records: Arc<Mutex<Vec<ApplyRecord>>> = Arc::new(Mutex::new(Vec::new()));
  let c_snapshots: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));

  let live_states = std::collections::HashMap::from([
    ("a", json!({"id": "a-1"})),
    ("b", json!({"id": "b-2"})),
  ]);
  let snapshot_map =
    std::collections::HashMap::from([("c", Arc::clone(&c_snapshots))]);

  let root = json!({
    "a": {
      "__nixhapi": {
        "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}}
      }
    },
    "b": {
      "__nixhapi": {
        "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}},
        "dependsOn": [r#".["a"]"#]
      },
      "fromA": {
        "__nixhapi": "derived-from",
        "inputs": {"aid": r#".["a"].id"#},
        "expression": "mkManaged(.aid)"
      }
    },
    "c": {
      "__nixhapi": {
        "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}},
        "dependsOn": [r#".["b"]"#]
      },
      "fromB": {
        "__nixhapi": "derived-from",
        "inputs": {"bid": r#".["b"].id"#},
        "expression": "mkManaged(.bid)"
      }
    }
  });

  execute_apply_waves(
    &root,
    make_resolver(&records, live_states, snapshot_map),
  )
  .expect("chained derivedFrom should succeed");

  let snaps = c_snapshots.lock().unwrap();
  assert_eq!(snaps.len(), 1);
  let desired = &snaps[0];
  assert_eq!(desired["fromB"]["__nixhapi"], json!("managed"));
  assert_eq!(desired["fromB"]["value"], json!("b-2"));
}

/// Diamond pattern with derivedFrom: two independent branches both read from
/// the root instance, then a sink reads from both branches.
///
/// ```text
/// wave 0: a  (live: {"uid": "user-1"})
/// wave 1: b  (DerivedFrom reads a.uid; live: {"team": "eng"})
///         c  (DerivedFrom reads a.uid; live: {"dept": "rd"})
/// wave 2: d  (DerivedFrom reads b.team and c.dept)
/// ```
#[test]
fn diamond_derived_from_resolved_across_waves() {
  let records: Arc<Mutex<Vec<ApplyRecord>>> = Arc::new(Mutex::new(Vec::new()));
  let d_snapshots: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));

  let live_states = std::collections::HashMap::from([
    ("a", json!({"uid": "user-1"})),
    ("b", json!({"team": "eng"})),
    ("c", json!({"dept": "rd"})),
  ]);
  let snapshot_map =
    std::collections::HashMap::from([("d", Arc::clone(&d_snapshots))]);

  let root = json!({
    "a": {
      "__nixhapi": {
        "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}}
      }
    },
    "b": {
      "__nixhapi": {
        "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}},
        "dependsOn": [r#".["a"]"#]
      },
      "uid": {
        "__nixhapi": "derived-from",
        "inputs": {"uid": r#".["a"].uid"#},
        "expression": "mkManaged(.uid)"
      }
    },
    "c": {
      "__nixhapi": {
        "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}},
        "dependsOn": [r#".["a"]"#]
      },
      "uid": {
        "__nixhapi": "derived-from",
        "inputs": {"uid": r#".["a"].uid"#},
        "expression": "mkManaged(.uid)"
      }
    },
    "d": {
      "__nixhapi": {
        "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}},
        "dependsOn": [r#".["b"]"#, r#".["c"]"#]
      },
      "team": {
        "__nixhapi": "derived-from",
        "inputs": {"t": r#".["b"].team"#},
        "expression": "mkManaged(.t)"
      },
      "dept": {
        "__nixhapi": "derived-from",
        "inputs": {"d": r#".["c"].dept"#},
        "expression": "mkManaged(.d)"
      }
    }
  });

  execute_apply_waves(
    &root,
    make_resolver(&records, live_states, snapshot_map),
  )
  .expect("diamond derivedFrom should succeed");

  let snaps = d_snapshots.lock().unwrap();
  assert_eq!(snaps.len(), 1);
  let desired = &snaps[0];
  assert_eq!(desired["team"]["__nixhapi"], json!("managed"));
  assert_eq!(desired["team"]["value"], json!("eng"));
  assert_eq!(desired["dept"]["__nixhapi"], json!("managed"));
  assert_eq!(desired["dept"]["value"], json!("rd"));
}
