use crate::field_value::{FieldValueError, ResolvedFieldValue};
use crate::meta::NixHapiMeta;
use crate::plan::{ApplyReport, ProviderPlan};
use std::collections::HashMap;
use thiserror::Error;

/// Resolved provider configuration: all `FieldValue`s have been read from
/// paths or environment variables.
pub type ResolvedConfig = HashMap<String, ResolvedFieldValue>;

/// Resolves every `FieldValue` in a raw config map into a `ResolvedConfig`.
pub fn resolve_config(
  raw: &HashMap<String, crate::field_value::FieldValue>,
) -> Result<ResolvedConfig, ProviderError> {
  raw
    .iter()
    .map(|(key, fv)| {
      fv.resolve()
        .map_err(|source| ProviderError::ConfigResolution {
          field: key.clone(),
          source,
        })
        .map(|resolved| (key.clone(), resolved))
    })
    .collect()
}

#[derive(Debug, Error)]
pub enum ProviderError {
  #[error(
    "Failed to resolve provider configuration field {field:?}: {source}"
  )]
  ConfigResolution {
    field: String,
    #[source]
    source: FieldValueError,
  },

  #[error("Required provider configuration field {field:?} is missing")]
  MissingConfig { field: String },

  #[error("Provider configuration field {field:?} must not be Unmanaged")]
  UnmanagedConfig { field: String },

  #[error("Failed to connect to provider: {0}")]
  ConnectionFailed(String),

  #[error("Provider operation failed: {0}")]
  OperationFailed(String),

  #[error("Failed to parse desired state: {0}")]
  DesiredStateParse(String),

  #[error("Failed to parse live state: {0}")]
  LiveStateParse(String),
}

/// A predicate restricting which live resources are considered owned.
///
/// No variants are defined in this release.  The parameter is present in
/// the provider trait so that filter support can be added without a
/// breaking API change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Filter {}

/// A provider implements the query-diff-apply loop for one API backend.
pub trait Provider: Send + Sync {
  /// Unique identifier for this provider type (e.g. `"ldap"`).
  fn provider_type(&self) -> &str;

  /// Config field names whose values must be scrubbed from plan output.
  fn sensitive_config_fields(&self) -> &[&str];

  /// Query all live resources within this provider's scope.
  ///
  /// `filters` is empty in this release; the parameter exists so filter
  /// support can be added without changing the trait signature.
  fn list_live(
    &self,
    config: &ResolvedConfig,
    filters: &[Filter],
  ) -> Result<serde_json::Value, ProviderError>;

  /// Compute what would change if the desired state were applied.
  ///
  /// `desired` is the provider's subtree from the Nix-generated JSON, with
  /// the `__nixhapi` key already stripped.  `live` is the value returned by
  /// a prior call to `list_live`.
  fn plan(
    &self,
    desired: &serde_json::Value,
    live: &serde_json::Value,
    meta: &NixHapiMeta,
    config: &ResolvedConfig,
  ) -> Result<ProviderPlan, ProviderError>;

  /// Execute the plan produced by `plan`.
  fn apply(
    &self,
    plan: &ProviderPlan,
    config: &ResolvedConfig,
  ) -> Result<ApplyReport, ProviderError>;
}
