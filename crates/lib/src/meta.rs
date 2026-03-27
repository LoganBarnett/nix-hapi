use crate::field_value::FieldValue;
use serde::Deserialize;
use std::collections::HashMap;

/// Directives from the `__nixhapi` key that modify reconciliation behaviour
/// for the enclosing scope.  The reconciler strips this key before passing
/// the subtree to the provider.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NixHapiMeta {
  /// Provider declaration for the top-level scope.
  pub provider: Option<ProviderSpec>,

  /// Regex patterns exempting resources from ownership.  Resources whose
  /// identifiers match any pattern are not deleted when absent from the
  /// desired state.
  #[serde(default)]
  pub ignore: Vec<String>,

  /// Execution order for cross-provider sequencing.  Steps with equal order
  /// may eventually run concurrently; lower order runs before higher.
  /// Defaults to 0 when absent.
  #[serde(default)]
  pub order: u32,
  // listFilter: reserved for next phase
}

/// Provider type and its connection/authentication configuration, all
/// expressed as `FieldValue`s so credentials can come from paths or
/// environment variables.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSpec {
  /// Provider type identifier (e.g. `"ldap"`).
  #[serde(rename = "type")]
  pub provider_type: String,

  /// Provider-specific configuration fields.  Every value is a `FieldValue`
  /// so it can reference a file path or environment variable.
  #[serde(flatten)]
  pub config: HashMap<String, FieldValue>,
}
