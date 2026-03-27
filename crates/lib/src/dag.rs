//! Dependency-graph resolution for provider instances.
//!
//! Each provider instance can declare `dependsOn` jq expressions that identify
//! other instances it requires to be fully applied first.  This module
//! evaluates those expressions and uses Kahn's topological-sort algorithm to
//! produce:
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
//! # `dependsOn` expressions
//!
//! Each entry in `__nixhapi.dependsOn` is a jq expression evaluated against
//! the full top-level JSON blob.  The result is matched by equality against
//! known provider scope values to identify the depended-upon instance.
//!
//! Example: given a top-level blob `{ "prod-ldap": {...}, "prod-dns": {...} }`,
//! the expression `.["prod-ldap"]` resolves to the `prod-ldap` instance.
//!
//! The Nix layer can compute the jq path from a config attribute reference,
//! making cross-provider dependencies first-class without hard-coding instance
//! names in application code.

use crate::meta::NixHapiMeta;
use serde_json::Value;
use std::collections::HashMap;
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

// ── Internal graph helpers ────────────────────────────────────────────────────

/// Builds the dependency map: each instance name maps to the list of instance
/// names it directly depends on.
fn build_dependency_map(
  top_level: &HashMap<String, Value>,
  full_json: &Value,
) -> Result<HashMap<String, Vec<String>>, DagError> {
  let mut deps_of: HashMap<String, Vec<String>> = HashMap::new();

  for (instance, scope) in top_level {
    let meta: NixHapiMeta = scope
      .get("__nixhapi")
      .and_then(|v| serde_json::from_value(v.clone()).ok())
      .unwrap_or_default();

    let mut deps = Vec::new();
    for expr in &meta.depends_on {
      let output = eval_jq_first(instance, expr, full_json.clone())?;
      let dep_name = top_level
        .iter()
        .find(|(_, v)| *v == &output)
        .map(|(name, _)| name.clone())
        .ok_or_else(|| DagError::UnresolvedDependency {
          instance: instance.clone(),
          expression: expr.clone(),
        })?;
      deps.push(dep_name);
    }
    deps_of.insert(instance.clone(), deps);
  }

  Ok(deps_of)
}

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
pub fn topological_order(
  top_level: &HashMap<String, Value>,
) -> Result<Vec<String>, DagError> {
  let full_json = Value::Object(
    top_level
      .iter()
      .map(|(k, v)| (k.clone(), v.clone()))
      .collect(),
  );
  let deps_of = build_dependency_map(top_level, &full_json)?;
  kahn_sort(&deps_of)
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
/// `Vec` when `top_level` is empty.
pub fn execution_waves(
  top_level: &HashMap<String, Value>,
) -> Result<Vec<Vec<String>>, DagError> {
  if top_level.is_empty() {
    return Ok(Vec::new());
  }

  let full_json = Value::Object(
    top_level
      .iter()
      .map(|(k, v)| (k.clone(), v.clone()))
      .collect(),
  );
  let deps_of = build_dependency_map(top_level, &full_json)?;
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
