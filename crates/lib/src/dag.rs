//! Dependency-graph resolution for provider instances.
//!
//! Each provider instance can declare `dependsOn` jq expressions that identify
//! other instances it requires to be fully applied first.  Additionally, any
//! [`FieldValue::DerivedFrom`] node anywhere in the desired-state tree creates
//! an implicit edge: the owning provider instance depends on every provider
//! instance that one of its `inputs` paths traverses into.
//!
//! This module evaluates both edge sources and uses Kahn's topological-sort
//! algorithm to produce:
//!
//! * A flat ordering for sequential processing ([`topological_order`]).
//! * A wave decomposition for parallel execution ([`execution_waves`]).
//!
//! # Wave execution model
//!
//! Providers that share no dependency relationship belong to the same wave and
//! may execute concurrently.  All instances in wave N must complete before any
//! instance in wave N+1 begins.
//!
//! ```text
//! wave 0 │  A      B        ← no dependencies; run in parallel
//!        │  │      │
//! wave 1 │  C      D        ← C←A, D←B; run in parallel
//!        │   ╲    ╱
//! wave 2 │     E            ← E←C, E←D; runs alone
//! ```
//!
//! # Edge sources
//!
//! **`dependsOn` (explicit):** each entry in `__nixhapi.dependsOn` is a jq
//! expression evaluated against the full top-level JSON blob.  The result is
//! matched by equality against known provider scope values to identify the
//! depended-upon instance.
//!
//! **`derivedFrom.inputs` (implicit):** each entry in the `inputs` map of a
//! `DerivedFrom` field is an absolute jq path of the form
//! `.[" instance-name "].rest...`.  The first path component names the provider
//! instance that must be applied before this field can be computed.

use crate::meta::NixHapiMeta;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use thiserror::Error;

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DagError {
  #[error(
    "dependsOn expression {expression:?} for instance {instance:?} \
     failed to parse: {message}"
  )]
  ExpressionParse {
    instance: String,
    expression: String,
    message: String,
  },

  #[error(
    "dependsOn expression {expression:?} for instance {instance:?} \
     failed to compile: {message}"
  )]
  ExpressionCompile {
    instance: String,
    expression: String,
    message: String,
  },

  #[error(
    "dependsOn expression {expression:?} for instance {instance:?} \
     produced no output"
  )]
  ExpressionNoOutput {
    instance: String,
    expression: String,
  },

  #[error(
    "dependsOn expression {expression:?} for instance {instance:?} \
     runtime error: {message}"
  )]
  ExpressionRuntime {
    instance: String,
    expression: String,
    message: String,
  },

  #[error(
    "dependsOn expression {expression:?} in instance {instance:?} \
     did not resolve to any known provider instance"
  )]
  UnresolvedDependency {
    instance: String,
    expression: String,
  },

  #[error(
    "derivedFrom input {alias:?} in instance {instance:?} has invalid path \
     {path:?}: must begin with .[\"<instance-name>\"]"
  )]
  InvalidInputPath {
    instance: String,
    alias: String,
    path: String,
  },

  #[error(
    "derivedFrom input {alias:?} in instance {instance:?} references \
     unknown provider instance {referenced:?}"
  )]
  UnresolvedInputDependency {
    instance: String,
    alias: String,
    referenced: String,
  },

  #[error("Cycle detected in provider dependency graph")]
  Cycle,
}

// ── jq evaluation ─────────────────────────────────────────────────────────────

/// Evaluates a jq expression against `input` and returns the first output.
fn eval_jq_first(
  instance: &str,
  expression: &str,
  input: Value,
) -> Result<Value, DagError> {
  use jaq_core::{load, Ctx, RcIter};
  use jaq_json::Val;

  let loader = load::Loader::new(jaq_std::defs().chain(jaq_json::defs()));
  let arena = load::Arena::default();

  let modules = loader
    .load(
      &arena,
      load::File {
        code: expression,
        path: "(expr)",
      },
    )
    .map_err(|errors| DagError::ExpressionParse {
      instance: instance.to_string(),
      expression: expression.to_string(),
      message: format!("{errors:?}"),
    })?;

  let inputs = RcIter::new(core::iter::empty::<Result<Val, _>>());

  let filter = jaq_core::Compiler::default()
    .with_funs(jaq_std::funs().chain(jaq_json::funs()))
    .compile(modules)
    .map_err(|errors| DagError::ExpressionCompile {
      instance: instance.to_string(),
      expression: expression.to_string(),
      message: format!("{errors:?}"),
    })?;

  // `inputs` must outlive `filter` — declared before, dropped after.
  let ctx = Ctx::new([], &inputs);
  let first = filter.run((ctx, Val::from(input))).next();

  match first {
    Some(Ok(val)) => Ok(Value::from(val)),
    Some(Err(e)) => Err(DagError::ExpressionRuntime {
      instance: instance.to_string(),
      expression: expression.to_string(),
      message: e.to_string(),
    }),
    None => Err(DagError::ExpressionNoOutput {
      instance: instance.to_string(),
      expression: expression.to_string(),
    }),
  }
}

// ── Input path parsing ────────────────────────────────────────────────────────

/// Extracts the top-level provider instance name from an absolute jq input
/// path of the form `.[" instance-name "]...`.  Returns `None` for any path
/// that does not begin with that bracketed pattern.
fn instance_from_input_path(path: &str) -> Option<&str> {
  path
    .strip_prefix(".[\"")
    .and_then(|s| s.find("\"]").map(|i| &s[..i]))
}

// ── Tree walk ─────────────────────────────────────────────────────────────────

/// Recursively walks `node` looking for edge-declaring constructs and
/// accumulates them into `deps_of[owning_instance]`.
///
/// Three node shapes are recognised:
/// - `__nixhapi` is an **object** → meta block; extract `dependsOn`.
///   Recurse into all children except the meta block itself.
/// - `__nixhapi` is a **string** `"derived-from"` → `DerivedFrom` leaf;
///   extract `inputs` and parse each path.  Do not recurse further.
/// - `__nixhapi` is any **other string** → other `FieldValue` leaf; skip.
/// - No `__nixhapi` key → plain data object; recurse into all children.
fn walk_for_edges(
  owning_instance: &str,
  node: &Value,
  root: &Value,
  known_instances: &HashSet<String>,
  deps_of: &mut HashMap<String, HashSet<String>>,
) -> Result<(), DagError> {
  let Some(obj) = node.as_object() else {
    return Ok(());
  };

  match obj.get("__nixhapi") {
    Some(Value::String(tag)) => {
      if tag == "derived-from" {
        let inputs = obj
          .get("inputs")
          .and_then(|v| v.as_object())
          .into_iter()
          .flatten();
        for (alias, path_val) in inputs {
          let path =
            path_val
              .as_str()
              .ok_or_else(|| DagError::InvalidInputPath {
                instance: owning_instance.to_string(),
                alias: alias.clone(),
                path: path_val.to_string(),
              })?;
          let dep = instance_from_input_path(path).ok_or_else(|| {
            DagError::InvalidInputPath {
              instance: owning_instance.to_string(),
              alias: alias.clone(),
              path: path.to_string(),
            }
          })?;
          if !known_instances.contains(dep) {
            return Err(DagError::UnresolvedInputDependency {
              instance: owning_instance.to_string(),
              alias: alias.clone(),
              referenced: dep.to_string(),
            });
          }
          // Self-references are valid at the field level but do not create
          // a cross-provider wave edge.
          if dep != owning_instance {
            deps_of
              .get_mut(owning_instance)
              .expect("owning instance in map")
              .insert(dep.to_string());
          }
        }
      }
      // Any FieldValue variant is a leaf; don't recurse into its children.
    }
    Some(Value::Object(_)) => {
      // Meta block: extract dependsOn, then recurse into data children only.
      let meta: NixHapiMeta =
        serde_json::from_value(obj["__nixhapi"].clone()).unwrap_or_default();
      for expr in &meta.depends_on {
        let output = eval_jq_first(owning_instance, expr, root.clone())?;
        let dep_name = root
          .as_object()
          .and_then(|top| top.iter().find(|(_, v)| *v == &output))
          .map(|(name, _)| name.clone())
          .ok_or_else(|| DagError::UnresolvedDependency {
            instance: owning_instance.to_string(),
            expression: expr.clone(),
          })?;
        if dep_name != owning_instance {
          deps_of
            .get_mut(owning_instance)
            .expect("owning instance in map")
            .insert(dep_name);
        }
      }
      for (key, child) in obj {
        if key == "__nixhapi" {
          continue;
        }
        walk_for_edges(owning_instance, child, root, known_instances, deps_of)?;
      }
    }
    _ => {
      // Plain data object: recurse into all children.
      for (_, child) in obj {
        walk_for_edges(owning_instance, child, root, known_instances, deps_of)?;
      }
    }
  }

  Ok(())
}

/// Collects the full dependency map by walking every top-level instance scope.
///
/// Returns a map from instance name to the sorted list of instance names it
/// directly depends on (deduplicated).
fn collect_edges(
  root: &Value,
) -> Result<HashMap<String, Vec<String>>, DagError> {
  let Some(top_obj) = root.as_object() else {
    return Ok(HashMap::new());
  };

  let known: HashSet<String> = top_obj.keys().cloned().collect();
  let mut deps_of: HashMap<String, HashSet<String>> = top_obj
    .keys()
    .map(|k| (k.clone(), HashSet::new()))
    .collect();

  for (instance_name, scope) in top_obj {
    walk_for_edges(instance_name, scope, root, &known, &mut deps_of)?;
  }

  Ok(
    deps_of
      .into_iter()
      .map(|(k, v)| {
        let mut deps: Vec<String> = v.into_iter().collect();
        deps.sort_unstable();
        (k, deps)
      })
      .collect(),
  )
}

// ── Kahn's algorithm ──────────────────────────────────────────────────────────

/// Kahn's algorithm over the dependency map.
///
/// Returns instance names in topological order.  Ties among nodes at the same
/// depth are broken alphabetically so results are deterministic regardless of
/// `HashMap` iteration order.  Returns [`DagError::Cycle`] if a cycle is
/// detected.
fn kahn_sort(
  deps_of: &HashMap<String, Vec<String>>,
) -> Result<Vec<String>, DagError> {
  let mut in_degree: HashMap<String, usize> =
    deps_of.keys().map(|k| (k.clone(), 0)).collect();
  let mut dependents: HashMap<String, Vec<String>> =
    deps_of.keys().map(|k| (k.clone(), Vec::new())).collect();

  for (instance, deps) in deps_of {
    for dep in deps {
      *in_degree.get_mut(instance).unwrap() += 1;
      dependents.get_mut(dep).unwrap().push(instance.clone());
    }
  }

  let mut queue: Vec<String> = in_degree
    .iter()
    .filter(|(_, &deg)| deg == 0)
    .map(|(k, _)| k.clone())
    .collect();
  queue.sort_unstable();

  let mut result: Vec<String> = Vec::new();
  while !queue.is_empty() {
    let node = queue.remove(0);
    if let Some(next_nodes) = dependents.get(&node) {
      let mut newly_ready: Vec<String> = next_nodes
        .iter()
        .filter_map(|dep| {
          let deg = in_degree.get_mut(dep).unwrap();
          *deg -= 1;
          (*deg == 0).then_some(dep.clone())
        })
        .collect();
      newly_ready.sort_unstable();
      queue.extend(newly_ready);
    }
    result.push(node);
  }

  if result.len() != deps_of.len() {
    return Err(DagError::Cycle);
  }

  Ok(result)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Returns provider instance names in topological dependency order.
///
/// Instances that have no unresolved prerequisites appear before those that
/// depend on them.  Among instances at the same dependency depth, ordering is
/// alphabetical for determinism.
///
/// Returns [`DagError::Cycle`] if the dependency graph contains a cycle.
pub fn topological_order(root: &Value) -> Result<Vec<String>, DagError> {
  kahn_sort(&collect_edges(root)?)
}

/// Groups provider instance names into sequential waves for parallel execution.
///
/// All instances within a wave are mutually independent and may execute
/// concurrently.  Wave N+1 must not begin until every instance in wave N has
/// completed successfully.
///
/// The wave index of an instance is:
/// - **0** — if it has no dependencies.
/// - `1 + max(wave index of each dependency)` — otherwise.
///
/// Instances within a wave are sorted alphabetically.  Returns an empty
/// `Vec` when `root` contains no instances.
pub fn execution_waves(root: &Value) -> Result<Vec<Vec<String>>, DagError> {
  let deps_of = collect_edges(root)?;
  if deps_of.is_empty() {
    return Ok(Vec::new());
  }

  let order = kahn_sort(&deps_of)?;

  // Process nodes in topological order so each node's dependencies are
  // already wave-assigned when it is reached.
  let mut wave_of: HashMap<String, usize> = HashMap::new();
  for node in &order {
    let wave = deps_of[node]
      .iter()
      .map(|dep| wave_of[dep] + 1)
      .max()
      .unwrap_or(0);
    wave_of.insert(node.clone(), wave);
  }

  let num_waves = wave_of.values().max().copied().unwrap_or(0) + 1;
  let mut waves: Vec<Vec<String>> = vec![Vec::new(); num_waves];
  for (node, &wave) in &wave_of {
    waves[wave].push(node.clone());
  }
  for wave in &mut waves {
    wave.sort_unstable();
  }

  Ok(waves)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  // Helper: a minimal provider scope with optional dependsOn.
  fn scope(depends_on: &[&str]) -> Value {
    json!({
      "__nixhapi": {
        "provider": {"type": "fake"},
        "dependsOn": depends_on
      }
    })
  }

  #[test]
  fn empty_root_gives_empty_waves() {
    let waves = execution_waves(&json!({})).unwrap();
    assert!(waves.is_empty());
  }

  #[test]
  fn single_instance_no_deps() {
    let root = json!({ "alpha": scope(&[]) });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves, vec![vec!["alpha"]]);
  }

  #[test]
  fn two_independent_instances_same_wave() {
    let root = json!({
      "alpha": scope(&[]),
      "beta":  scope(&[])
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves, vec![vec!["alpha", "beta"]]);
  }

  #[test]
  fn explicit_dep_creates_two_waves() {
    let root = json!({
      "a": scope(&[]),
      "b": scope(&[r#".["a"]"#])
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves[0], vec!["a"]);
    assert_eq!(waves[1], vec!["b"]);
  }

  #[test]
  fn diamond_dag_three_waves() {
    // a → {b, c} → d
    let root = json!({
      "a": scope(&[]),
      "b": scope(&[r#".["a"]"#]),
      "c": scope(&[r#".["a"]"#]),
      "d": scope(&[r#".["b"]"#, r#".["c"]"#])
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves[0], vec!["a"]);
    assert_eq!(waves[1], vec!["b", "c"]);
    assert_eq!(waves[2], vec!["d"]);
  }

  #[test]
  fn deep_chain_four_waves() {
    let root = json!({
      "a": scope(&[]),
      "b": scope(&[r#".["a"]"#]),
      "c": scope(&[r#".["b"]"#]),
      "d": scope(&[r#".["c"]"#])
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves.len(), 4);
    assert_eq!(waves[0], vec!["a"]);
    assert_eq!(waves[1], vec!["b"]);
    assert_eq!(waves[2], vec!["c"]);
    assert_eq!(waves[3], vec!["d"]);
  }

  #[test]
  fn cycle_returns_error() {
    let root = json!({
      "a": scope(&[r#".["b"]"#]),
      "b": scope(&[r#".["a"]"#])
    });
    assert!(matches!(execution_waves(&root), Err(DagError::Cycle)));
  }

  #[test]
  fn wave_members_sorted_alphabetically() {
    let root = json!({
      "zebra":   scope(&[]),
      "alpha":   scope(&[]),
      "mammoth": scope(&[])
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves, vec![vec!["alpha", "mammoth", "zebra"]]);
  }

  #[test]
  fn derived_from_creates_cross_instance_edge() {
    // scheduling.memberships.alice.userId is a derivedFrom that references
    // hr-system; no explicit dependsOn anywhere.
    let root = json!({
      "hr-system": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "users": {
          "alice": {
            "id": {"__nixhapi": "managed", "value": "user-123"}
          }
        }
      },
      "scheduling": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "memberships": {
          "alice-on-call": {
            "userId": {
              "__nixhapi": "derived-from",
              "inputs": {"uid": r#".["hr-system"].users.alice.id"#},
              "expression": "mkManaged($inputs.uid)"
            }
          }
        }
      }
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves[0], vec!["hr-system"]);
    assert_eq!(waves[1], vec!["scheduling"]);
  }

  #[test]
  fn derived_from_nested_deep_still_creates_edge() {
    // derivedFrom buried three levels inside the scope.
    let root = json!({
      "source": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "data": {"value": {"__nixhapi": "managed", "value": "x"}}
      },
      "consumer": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "level1": {
          "level2": {
            "level3": {
              "field": {
                "__nixhapi": "derived-from",
                "inputs": {"v": r#".["source"].data.value"#},
                "expression": "mkManaged($inputs.v)"
              }
            }
          }
        }
      }
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves[0], vec!["source"]);
    assert_eq!(waves[1], vec!["consumer"]);
  }

  #[test]
  fn derived_from_same_instance_no_cross_edge() {
    // A field within alpha derives from another field within alpha; no wave
    // promotion should occur.
    let root = json!({
      "alpha": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "field_a": {"__nixhapi": "managed", "value": "x"},
        "field_b": {
          "__nixhapi": "derived-from",
          "inputs": {"x": r#".["alpha"].field_a"#},
          "expression": "mkManaged($inputs.x)"
        }
      }
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves, vec![vec!["alpha"]]);
  }

  #[test]
  fn derived_from_and_depends_on_combined() {
    // C has an explicit dependsOn on B and a derivedFrom input on A.
    // B and A are independent → both in wave 0, C in wave 1.
    let root = json!({
      "a": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "out": {"__nixhapi": "managed", "value": "result"}
      },
      "b": scope(&[]),
      "c": {
        "__nixhapi": {
          "provider": {"type": "fake"},
          "dependsOn": [r#".["b"]"#]
        },
        "field": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["a"].out"#},
          "expression": "mkManaged($inputs.v)"
        }
      }
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves[0], vec!["a", "b"]);
    assert_eq!(waves[1], vec!["c"]);
  }

  #[test]
  fn derived_from_invalid_path_format_error() {
    let root = json!({
      "alpha": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "field": {
          "__nixhapi": "derived-from",
          "inputs": {"x": "not-a-valid-input-path"},
          "expression": "."
        }
      }
    });
    assert!(matches!(
      execution_waves(&root),
      Err(DagError::InvalidInputPath { .. })
    ));
  }

  #[test]
  fn derived_from_unknown_instance_error() {
    let root = json!({
      "alpha": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "field": {
          "__nixhapi": "derived-from",
          "inputs": {"x": r#".["ghost"].some.field"#},
          "expression": "."
        }
      }
    });
    assert!(matches!(
      execution_waves(&root),
      Err(DagError::UnresolvedInputDependency { .. })
    ));
  }

  #[test]
  fn cycle_via_derived_from() {
    // A depends on B via dependsOn; B has a derivedFrom input on A → cycle.
    let root = json!({
      "a": {
        "__nixhapi": {
          "provider": {"type": "fake"},
          "dependsOn": [r#".["b"]"#]
        }
      },
      "b": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "field": {
          "__nixhapi": "derived-from",
          "inputs": {"x": r#".["a"].something"#},
          "expression": "."
        }
      }
    });
    assert!(matches!(execution_waves(&root), Err(DagError::Cycle)));
  }

  #[test]
  fn topological_order_respects_dependencies() {
    let root = json!({
      "a": scope(&[]),
      "b": scope(&[r#".["a"]"#]),
      "c": scope(&[r#".["b"]"#])
    });
    let order = topological_order(&root).unwrap();
    let pos = |name: &str| order.iter().position(|s| s == name).unwrap();
    assert!(pos("a") < pos("b"));
    assert!(pos("b") < pos("c"));
  }

  #[test]
  fn instance_from_input_path_extracts_name() {
    assert_eq!(
      instance_from_input_path(r#".["hr-system"]"#),
      Some("hr-system")
    );
    assert_eq!(
      instance_from_input_path(r#".["hr-system"].users.alice.id"#),
      Some("hr-system")
    );
    assert_eq!(instance_from_input_path(".alpha"), None);
    assert_eq!(instance_from_input_path("bad-path"), None);
    assert_eq!(instance_from_input_path(""), None);
  }

  // ── derivedFrom chain and diamond ─────────────────────────────────────────

  // These tests verify that transitive ordering is an emergent property of
  // per-node edge collection: no special "chain" support is needed in the
  // algorithm — each node declares only its direct inputs and the DAG
  // naturally computes the full ordering.

  #[test]
  fn derived_from_chain_three_providers() {
    // a → b → c, all edges via derivedFrom, no dependsOn anywhere.
    // b's field references a's output; c's field references b's (still
    // DerivedFrom) field.  The DAG does not need to know that b's value is
    // itself derived — it only cares that c references something inside b's
    // scope, which creates the edge c→b.
    let root = json!({
      "a": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "output": {"__nixhapi": "managed", "value": "a-value"}
      },
      "b": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "derived": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["a"].output"#},
          "expression": "mkManaged($inputs.v)"
        }
      },
      "c": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "derived": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["b"].derived"#},
          "expression": "mkManaged($inputs.v)"
        }
      }
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves.len(), 3);
    assert_eq!(waves[0], vec!["a"]);
    assert_eq!(waves[1], vec!["b"]);
    assert_eq!(waves[2], vec!["c"]);
  }

  #[test]
  fn derived_from_diamond_four_providers() {
    // Diamond: a → {b, c} → d, all edges via derivedFrom.
    // d has a single field with two inputs — one from b and one from c —
    // which creates edges d→b and d→c.  Combined with b→a and c→a the full
    // diamond emerges from the graph.
    let root = json!({
      "a": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "output": {"__nixhapi": "managed", "value": "a-value"}
      },
      "b": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "derived": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["a"].output"#},
          "expression": "mkManaged($inputs.v)"
        }
      },
      "c": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "derived": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["a"].output"#},
          "expression": "mkManaged($inputs.v)"
        }
      },
      "d": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "derived": {
          "__nixhapi": "derived-from",
          "inputs": {
            "bv": r#".["b"].derived"#,
            "cv": r#".["c"].derived"#
          },
          "expression": "mkManaged($inputs.bv)"
        }
      }
    });
    let waves = execution_waves(&root).unwrap();
    assert_eq!(waves[0], vec!["a"]);
    assert_eq!(waves[1], vec!["b", "c"]);
    assert_eq!(waves[2], vec!["d"]);
  }

  // ── cycle detection across both edge mechanisms ────────────────────────────

  // cycle_via_derived_from (above) has: a dependsOn b AND b derivedFrom a.
  // This test covers the mirror: a derivedFrom b AND b dependsOn a.
  // Both arrangements must be detected as cycles regardless of which
  // mechanism introduces each half of the loop.
  #[test]
  fn cycle_derived_from_and_depends_on_reversed() {
    let root = json!({
      "a": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "field": {
          "__nixhapi": "derived-from",
          "inputs": {"x": r#".["b"].output"#},
          "expression": "mkManaged($inputs.x)"
        }
      },
      "b": {
        "__nixhapi": {
          "provider": {"type": "fake"},
          "dependsOn": [r#".["a"]"#]
        },
        "output": {"__nixhapi": "managed", "value": "b-value"}
      }
    });
    assert!(matches!(execution_waves(&root), Err(DagError::Cycle)));
  }
}
