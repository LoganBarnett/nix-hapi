use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FieldValueError {
  #[error("Failed to read field value from path {path:?}: {source}")]
  PathRead {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },

  #[error(
    "Failed to read field value from environment variable {env:?}: {source}"
  )]
  EnvRead {
    env: String,
    #[source]
    source: std::env::VarError,
  },
}

/// How a field value is managed and where its value comes from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "__nixhapi", rename_all = "kebab-case")]
pub enum FieldValue {
  /// Always enforce this exact value on every reconciliation.
  Managed { value: String },
  /// Set once if absent; leave alone once set.
  Initial { value: String },
  /// Never touch this field.  Documents intentional non-ownership.
  Unmanaged,
  /// Read value from a file path on every reconciliation, always enforce.
  ManagedFromPath { path: PathBuf },
  /// Read value from a file path; set once if absent.
  InitialFromPath { path: PathBuf },
  /// Read value from an environment variable on every reconciliation, always enforce.
  ManagedFromEnv { env: String },
  /// Read value from an environment variable; set once if absent.
  InitialFromEnv { env: String },
}

impl FieldValue {
  pub fn resolve(&self) -> Result<ResolvedFieldValue, FieldValueError> {
    match self {
      FieldValue::Managed { value } => {
        Ok(ResolvedFieldValue::Managed(value.clone()))
      }
      FieldValue::Initial { value } => {
        Ok(ResolvedFieldValue::Initial(value.clone()))
      }
      FieldValue::Unmanaged => Ok(ResolvedFieldValue::Unmanaged),
      FieldValue::ManagedFromPath { path } => std::fs::read_to_string(path)
        .map_err(|source| FieldValueError::PathRead {
          path: path.clone(),
          source,
        })
        .map(|s| ResolvedFieldValue::Managed(s.trim().to_string())),
      FieldValue::InitialFromPath { path } => std::fs::read_to_string(path)
        .map_err(|source| FieldValueError::PathRead {
          path: path.clone(),
          source,
        })
        .map(|s| ResolvedFieldValue::Initial(s.trim().to_string())),
      FieldValue::ManagedFromEnv { env } => std::env::var(env)
        .map_err(|source| FieldValueError::EnvRead {
          env: env.clone(),
          source,
        })
        .map(ResolvedFieldValue::Managed),
      FieldValue::InitialFromEnv { env } => std::env::var(env)
        .map_err(|source| FieldValueError::EnvRead {
          env: env.clone(),
          source,
        })
        .map(ResolvedFieldValue::Initial),
    }
  }
}

/// A field value with all sources resolved to concrete strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedFieldValue {
  Managed(String),
  Initial(String),
  Unmanaged,
}

impl ResolvedFieldValue {
  /// The resolved string value, or `None` for `Unmanaged`.
  pub fn value(&self) -> Option<&str> {
    match self {
      ResolvedFieldValue::Managed(v) | ResolvedFieldValue::Initial(v) => {
        Some(v.as_str())
      }
      ResolvedFieldValue::Unmanaged => None,
    }
  }

  pub fn is_managed(&self) -> bool {
    matches!(self, ResolvedFieldValue::Managed(_))
  }

  pub fn is_initial(&self) -> bool {
    matches!(self, ResolvedFieldValue::Initial(_))
  }

  pub fn is_unmanaged(&self) -> bool {
    matches!(self, ResolvedFieldValue::Unmanaged)
  }
}
