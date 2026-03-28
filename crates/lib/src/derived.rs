//! Runtime resolution of [`crate::field_value::FieldValue::DerivedFrom`] nodes.
//!
//! During wave-parallel execution the executor builds a *crystalized* tree:
//! after each wave's apply, `list_live` is called for every instance in the
//! wave and the results are inserted at the instance's key in the crystalized
//! tree.  Between waves, [`resolve_derived_from_tree`] walks the active
//! desired-state tree and replaces every `DerivedFrom` node whose inputs are
//! all present in the crystalized tree.
//!
//! Nodes whose inputs are not yet available are left unchanged and will be
//! revisited after the next wave completes.

use crate::dag::{eval_jq_first, DagError};
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DerivedFromError {
  #[error("Failed to evaluate derivedFrom input path {path:?}: {source}")]
  InputPathEval {
    path: String,
    #[source]
    source: DagError,
  },

  #[error(
    "Failed to evaluate derivedFrom expression {expression:?}: {source}"
  )]
  ExpressionEval {
    expression: String,
    #[source]
    source: DagError,
  },
}

// ── mk* helpers ───────────────────────────────────────────────────────────────

/// jq definitions for the `mk*` helper functions available in every
/// `derivedFrom` expression.  These produce `FieldValue`-shaped JSON objects
/// that the executor can deserialise back into the tree.
const MK_HELPERS: &str = r#"def mkManaged(v): {"__nixhapi": "managed", "value": v};
def mkInitial(v): {"__nixhapi": "initial", "value": v};
def mkUnmanaged: {"__nixhapi": "unmanaged"};
def mkManagedFromPath(p): {"__nixhapi": "managed-from-path", "path": p};
def mkInitialFromPath(p): {"__nixhapi": "initial-from-path", "path": p};
def mkManagedFromEnv(e): {"__nixhapi": "managed-from-env", "env": e};
def mkInitialFromEnv(e): {"__nixhapi": "initial-from-env", "env": e};"#;

// ── Private helpers ───────────────────────────────────────────────────────────

/// Evaluates a single input path against the crystalized tree.
///
/// Returns `None` when the path produces `null` or when the path traverses
/// through a missing key (e.g. the instance has not been applied yet).
/// Wrapping in `try … catch null` converts jaq navigation-through-null
/// errors into a graceful "not yet available" signal.
fn eval_input_path(
  path: &str,
  crystalized: &Value,
) -> Result<Option<Value>, DerivedFromError> {
  let safe = format!("try ({path}) catch null");
  let result = eval_jq_first("(derived-from)", &safe, crystalized.clone())
    .map_err(|source| DerivedFromError::InputPathEval {
      path: path.to_string(),
      source,
    })?;
  Ok(if result == Value::Null {
    None
  } else {
    Some(result)
  })
}

/// Evaluates a `derivedFrom` expression with the resolved inputs as `.`.
///
/// The `mk*` helper definitions are prepended so expressions can call
/// `mkManaged(.uid)` and similar without any preamble.
fn eval_derived_expression(
  expression: &str,
  inputs_value: Value,
) -> Result<Value, DerivedFromError> {
  let full = format!("{}\n{}", MK_HELPERS, expression);
  eval_jq_first("(derived-from)", &full, inputs_value).map_err(|source| {
    DerivedFromError::ExpressionEval {
      expression: expression.to_string(),
      source,
    }
  })
}

/// Attempts to resolve a single `DerivedFrom` node against `crystalized`.
///
/// Returns `Ok(None)` if any input evaluates to null (not yet available).
fn try_resolve_node(
  inputs: &HashMap<String, String>,
  expression: &str,
  crystalized: &Value,
) -> Result<Option<Value>, DerivedFromError> {
  let mut inputs_obj = serde_json::Map::new();
  for (alias, path) in inputs {
    match eval_input_path(path, crystalized)? {
      None => return Ok(None),
      Some(val) => {
        inputs_obj.insert(alias.clone(), val);
      }
    }
  }
  eval_derived_expression(expression, Value::Object(inputs_obj)).map(Some)
}

/// Recursively walks `node`, replacing resolvable `DerivedFrom` leaves.
fn resolve_value(
  node: Value,
  crystalized: &Value,
) -> Result<Value, DerivedFromError> {
  match node {
    Value::Array(arr) => arr
      .into_iter()
      .map(|v| resolve_value(v, crystalized))
      .collect::<Result<Vec<_>, _>>()
      .map(Value::Array),
    Value::Object(obj) => {
      let tag = obj
        .get("__nixhapi")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
      match tag.as_deref() {
        Some("derived-from") => {
          let inputs: HashMap<String, String> = obj
            .get("inputs")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
          let expression = obj
            .get("expression")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
          match try_resolve_node(&inputs, &expression, crystalized)? {
            Some(resolved) => Ok(resolved),
            // Not yet resolvable; leave the node intact for the next wave.
            None => Ok(Value::Object(obj)),
          }
        }
        // Any other FieldValue variant is a leaf; do not recurse.
        Some(_) => Ok(Value::Object(obj)),
        // Plain data object: recurse into all children.
        None => obj
          .into_iter()
          .map(|(k, v)| resolve_value(v, crystalized).map(|rv| (k, rv)))
          .collect::<Result<serde_json::Map<_, _>, _>>()
          .map(Value::Object),
      }
    }
    other => Ok(other),
  }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Walks `desired` and replaces every `DerivedFrom` node whose inputs are all
/// present in `crystalized`.
///
/// `crystalized` is a JSON object mapping provider instance names to the live
/// state returned by their most recent `list_live` call.  Nodes whose inputs
/// reference an instance that has not yet been applied (absent in
/// `crystalized` or evaluating to `null`) are left unchanged.
pub fn resolve_derived_from_tree(
  desired: &Value,
  crystalized: &Value,
) -> Result<Value, DerivedFromError> {
  resolve_value(desired.clone(), crystalized)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn crystalized_with(instance: &str, state: Value) -> Value {
    json!({ instance: state })
  }

  #[test]
  fn empty_crystalized_leaves_node_unchanged() {
    let desired = json!({
      "a": {
        "__nixhapi": {
          "provider": {"type": "fake", "delayMs": {"__nixhapi": "managed", "value": "0"}}
        },
        "userId": {
          "__nixhapi": "derived-from",
          "inputs": {"uid": r#".["b"].user.id"#},
          "expression": "mkManaged(.uid)"
        }
      }
    });
    let result = resolve_derived_from_tree(&desired, &json!({})).unwrap();
    assert_eq!(result["a"]["userId"]["__nixhapi"], json!("derived-from"));
  }

  #[test]
  fn resolved_node_replaced_with_field_value() {
    let crystalized = crystalized_with("b", json!({"user": {"id": "uid-42"}}));
    let desired = json!({
      "a": {
        "userId": {
          "__nixhapi": "derived-from",
          "inputs": {"uid": r#".["b"].user.id"#},
          "expression": "mkManaged(.uid)"
        }
      }
    });
    let result = resolve_derived_from_tree(&desired, &crystalized).unwrap();
    assert_eq!(result["a"]["userId"]["__nixhapi"], json!("managed"));
    assert_eq!(result["a"]["userId"]["value"], json!("uid-42"));
  }

  #[test]
  fn partially_missing_inputs_leaves_node_unchanged() {
    // One input resolved, one not → node stays in place.
    let crystalized = crystalized_with("b", json!({"user": {"id": "uid-42"}}));
    let desired = json!({
      "a": {
        "field": {
          "__nixhapi": "derived-from",
          "inputs": {
            "uid": r#".["b"].user.id"#,
            "sched": r#".["c"].schedule.id"#
          },
          "expression": r#"mkManaged(.uid + ":" + .sched)"#
        }
      }
    });
    let result = resolve_derived_from_tree(&desired, &crystalized).unwrap();
    assert_eq!(result["a"]["field"]["__nixhapi"], json!("derived-from"));
  }

  #[test]
  fn mk_helpers_produce_correct_field_value_shapes() {
    let crystalized = crystalized_with("src", json!({"val": "hello"}));
    let desired = json!({
      "dst": {
        "managed_field": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["src"].val"#},
          "expression": "mkManaged(.v)"
        },
        "initial_field": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["src"].val"#},
          "expression": "mkInitial(.v)"
        },
        "unmanaged_field": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["src"].val"#},
          "expression": "mkUnmanaged"
        }
      }
    });
    let result = resolve_derived_from_tree(&desired, &crystalized).unwrap();
    assert_eq!(
      result["dst"]["managed_field"],
      json!({"__nixhapi": "managed", "value": "hello"})
    );
    assert_eq!(
      result["dst"]["initial_field"],
      json!({"__nixhapi": "initial", "value": "hello"})
    );
    assert_eq!(
      result["dst"]["unmanaged_field"],
      json!({"__nixhapi": "unmanaged"})
    );
  }

  #[test]
  fn non_derived_from_nodes_pass_through_unchanged() {
    let desired = json!({
      "a": {
        "name": {"__nixhapi": "managed", "value": "alice"},
        "pass": {"__nixhapi": "initial", "value": "secret"},
        "note": {"__nixhapi": "unmanaged"}
      }
    });
    let result = resolve_derived_from_tree(&desired, &json!({})).unwrap();
    assert_eq!(result, desired);
  }

  #[test]
  fn nested_derived_from_inside_plain_object_resolved() {
    let crystalized = crystalized_with("src", json!({"x": "42"}));
    let desired = json!({
      "dst": {
        "group": {
          "nested": {
            "__nixhapi": "derived-from",
            "inputs": {"x": r#".["src"].x"#},
            "expression": "mkManaged(.x)"
          }
        }
      }
    });
    let result = resolve_derived_from_tree(&desired, &crystalized).unwrap();
    assert_eq!(result["dst"]["group"]["nested"]["__nixhapi"], json!("managed"));
    assert_eq!(result["dst"]["group"]["nested"]["value"], json!("42"));
  }

  #[test]
  fn multiple_inputs_combined_in_expression() {
    let crystalized = json!({
      "a": {"first": "Alice"},
      "b": {"last": "Smith"}
    });
    let desired = json!({
      "c": {
        "full_name": {
          "__nixhapi": "derived-from",
          "inputs": {
            "first": r#".["a"].first"#,
            "last": r#".["b"].last"#
          },
          "expression": r#"mkManaged(.first + " " + .last)"#
        }
      }
    });
    let result = resolve_derived_from_tree(&desired, &crystalized).unwrap();
    assert_eq!(result["c"]["full_name"]["__nixhapi"], json!("managed"));
    assert_eq!(result["c"]["full_name"]["value"], json!("Alice Smith"));
  }
}
