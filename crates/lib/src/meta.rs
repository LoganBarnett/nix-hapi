use crate::field_value::FieldValue;
use crate::jq_expr::JqExpr;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Directives from the `__nixhapi` key that modify reconciliation behaviour
/// for the enclosing scope.  The reconciler strips this key before passing
/// the subtree to the provider.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NixHapiMeta {
  /// Provider declaration for the top-level scope.
  pub provider: Option<ProviderSpec>,

  /// jq filter expressions exempting resources from ownership.  Resources
  /// for which any expression evaluates to truthy are not deleted when
  /// absent from the desired state.  Accepts plain strings (sugar) or
  /// structured `JqExpr` objects.
  #[serde(default)]
  pub ignore: Vec<JqExpr>,

  /// jq expressions that each evaluate to a provider-instance subtree that
  /// must be fully applied before this instance begins.  The expressions are
  /// evaluated against the complete top-level JSON blob, so cross-provider
  /// references work naturally.  Accepts plain strings (sugar) or structured
  /// `JqExpr` objects.
  #[serde(default)]
  pub depends_on: Vec<JqExpr>,
  // listFilter: reserved for next phase
}

/// Provider type and its connection/authentication configuration, all
/// expressed as `FieldValue`s so credentials can come from paths or
/// environment variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSpec {
  /// Provider type identifier (e.g. `"ldap"`).
  #[serde(rename = "type")]
  pub provider_type: String,

  /// Provider-specific configuration fields.  Every value is a `FieldValue`
  /// so it can reference a file path or environment variable.
  #[serde(flatten)]
  pub config: HashMap<String, FieldValue>,
}
