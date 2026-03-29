//! Plan-time structural reachability validation for `DerivedFrom` inputs.
//!
//! Before executing any wave, [`check_derived_from_saturation`] walks the full
//! desired-state tree and verifies that every `DerivedFrom` node's inputs
//! reference instances that will be available in an earlier wave.  This catches
//! ordering problems at plan time rather than silently leaving `DerivedFrom`
//! fields unresolved.

use crate::dag::instance_from_input_path;
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SaturationError {
  #[error(
    "DerivedFrom at {field_path:?}: input {input_alias:?} references \
     instance {referenced_instance:?} which is in wave {referenced_wave} \
     (same or later than owning instance in wave {owning_wave})"
  )]
  InputReferencesLaterWave {
    field_path: String,
    input_alias: String,
    referenced_instance: String,
    referenced_wave: usize,
    owning_wave: usize,
  },

  #[error(
    "DerivedFrom at {field_path:?}: input {input_alias:?} references \
     unknown instance {referenced_instance:?}"
  )]
  InputReferencesMissingInstance {
    field_path: String,
    input_alias: String,
    referenced_instance: String,
  },

  #[error(
    "DerivedFrom at {field_path:?}: input {input_alias:?} has invalid \
     path {path:?} — must begin with .[\"<instance-name>\"]"
  )]
  InvalidInputPath {
    field_path: String,
    input_alias: String,
    path: String,
  },
}

/// A `DerivedFrom` node discovered during the tree walk.
struct DerivedFromNode {
  /// Dotted path to this node in the tree (for error messages).
  field_path: String,
  /// The owning top-level instance name.
  owning_instance: String,
  /// Input alias → absolute jq path.
  inputs: HashMap<String, String>,
}

/// Recursively walks `node` collecting all `DerivedFrom` entries.
fn collect_derived_from_nodes(
  node: &Value,
  owning_instance: &str,
  current_path: &str,
  out: &mut Vec<DerivedFromNode>,
) {
  let Some(obj) = node.as_object() else {
    return;
  };

  match obj.get("__nixhapi") {
    Some(Value::String(tag)) if tag == "derived-from" => {
      let inputs: HashMap<String, String> = obj
        .get("inputs")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
      out.push(DerivedFromNode {
        field_path: current_path.to_string(),
        owning_instance: owning_instance.to_string(),
        inputs,
      });
    }
    // Other FieldValue leaves — skip.
    Some(Value::String(_)) => {}
    // Meta block or plain data — recurse into children.
    _ => {
      for (key, child) in obj {
        if key == "__nixhapi" {
          continue;
        }
        let child_path = if current_path.is_empty() {
          key.clone()
        } else {
          format!("{current_path}.{key}")
        };
        collect_derived_from_nodes(child, owning_instance, &child_path, out);
      }
    }
  }
}

/// Validates that every `DerivedFrom` input references an instance in an
/// earlier wave than the owning instance.
///
/// Same-instance references (a field that derives from another field in its
/// own instance) are allowed and skip the wave check.
pub fn check_derived_from_saturation(
  root: &Value,
  waves: &[Vec<String>],
) -> Result<(), SaturationError> {
  // Build instance → wave index map.
  let wave_of: HashMap<&str, usize> = waves
    .iter()
    .enumerate()
    .flat_map(|(i, wave)| wave.iter().map(move |name| (name.as_str(), i)))
    .collect();

  let Some(top) = root.as_object() else {
    return Ok(());
  };

  let mut nodes = Vec::new();
  for (instance_name, scope) in top {
    collect_derived_from_nodes(scope, instance_name, instance_name, &mut nodes);
  }

  for node in &nodes {
    let owning_wave = wave_of
      .get(node.owning_instance.as_str())
      .copied()
      .unwrap_or(0);

    for (alias, path) in &node.inputs {
      let referenced = instance_from_input_path(path).ok_or_else(|| {
        SaturationError::InvalidInputPath {
          field_path: node.field_path.clone(),
          input_alias: alias.clone(),
          path: path.clone(),
        }
      })?;

      // Self-references are valid; no wave ordering needed.
      if referenced == node.owning_instance {
        continue;
      }

      let referenced_wave =
        wave_of.get(referenced).copied().ok_or_else(|| {
          SaturationError::InputReferencesMissingInstance {
            field_path: node.field_path.clone(),
            input_alias: alias.clone(),
            referenced_instance: referenced.to_string(),
          }
        })?;

      if referenced_wave >= owning_wave {
        return Err(SaturationError::InputReferencesLaterWave {
          field_path: node.field_path.clone(),
          input_alias: alias.clone(),
          referenced_instance: referenced.to_string(),
          referenced_wave,
          owning_wave,
        });
      }
    }
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn valid_diamond_across_waves() {
    let root = json!({
      "a": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "output": {"__nixhapi": "managed", "value": "x"}
      },
      "b": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "field": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["a"].output"#},
          "expression": "mkManaged(.v)"
        }
      }
    });
    let waves = vec![vec!["a".to_string()], vec!["b".to_string()]];
    assert!(check_derived_from_saturation(&root, &waves).is_ok());
  }

  #[test]
  fn same_wave_reference_is_error() {
    let root = json!({
      "a": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "output": {"__nixhapi": "managed", "value": "x"}
      },
      "b": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "field": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["a"].output"#},
          "expression": "mkManaged(.v)"
        }
      }
    });
    // Artificially place both in the same wave.
    let waves = vec![vec!["a".to_string(), "b".to_string()]];
    assert!(matches!(
      check_derived_from_saturation(&root, &waves),
      Err(SaturationError::InputReferencesLaterWave { .. })
    ));
  }

  #[test]
  fn missing_instance_reference_is_error() {
    let root = json!({
      "a": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "field": {
          "__nixhapi": "derived-from",
          "inputs": {"v": r#".["ghost"].output"#},
          "expression": "mkManaged(.v)"
        }
      }
    });
    let waves = vec![vec!["a".to_string()]];
    assert!(matches!(
      check_derived_from_saturation(&root, &waves),
      Err(SaturationError::InputReferencesMissingInstance { .. })
    ));
  }

  #[test]
  fn self_reference_is_allowed() {
    let root = json!({
      "a": {
        "__nixhapi": {"provider": {"type": "fake"}, "dependsOn": []},
        "field_a": {"__nixhapi": "managed", "value": "x"},
        "field_b": {
          "__nixhapi": "derived-from",
          "inputs": {"x": r#".["a"].field_a"#},
          "expression": "mkManaged(.x)"
        }
      }
    });
    let waves = vec![vec!["a".to_string()]];
    assert!(check_derived_from_saturation(&root, &waves).is_ok());
  }
}
