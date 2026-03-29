//! A jq expression type that supports inline strings and file references.
//!
//! Anywhere nix-hapi expects a jq expression (ignores, `dependsOn`, future
//! filter expressions), this type accepts either a plain Nix string (sugar) or
//! a structured object pointing to a file.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JqExprError {
  #[error("Failed to read jq expression from file {path:?}: {source}")]
  FileRead {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },
}

/// A jq expression, either inline or loaded from a file.
///
/// A plain JSON string deserializes as `Inline` (the sugar path).  A
/// structured object with a `__nixhapi` tag provides explicit inline or
/// file-based expressions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum JqExpr {
  /// A plain inline jq expression string.
  Inline(String),
  /// A structured jq expression with a `__nixhapi` tag.
  Structured(JqExprStructured),
}

/// Structured jq expression variants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "__nixhapi")]
pub enum JqExprStructured {
  /// An inline jq expression carried in a structured object.
  #[serde(rename = "jq-expr")]
  Inline { value: String },
  /// A jq expression read from a file at resolve time.
  #[serde(rename = "jq-file")]
  File { path: PathBuf },
}

impl JqExpr {
  /// Resolves the expression to a jq source string.
  ///
  /// For inline variants, returns the string directly.  For file variants,
  /// reads the file from disk.
  pub fn resolve(&self) -> Result<String, JqExprError> {
    match self {
      JqExpr::Inline(s) => Ok(s.clone()),
      JqExpr::Structured(JqExprStructured::Inline { value }) => {
        Ok(value.clone())
      }
      JqExpr::Structured(JqExprStructured::File { path }) => {
        std::fs::read_to_string(path)
          .map(|s| s.trim().to_string())
          .map_err(|source| JqExprError::FileRead {
            path: path.clone(),
            source,
          })
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn bare_string_deserializes_as_inline() {
    let expr: JqExpr = serde_json::from_str(r#"".[\"a\"]""#).unwrap();
    assert_eq!(expr, JqExpr::Inline(r#".["a"]"#.to_string()));
    assert_eq!(expr.resolve().unwrap(), r#".["a"]"#);
  }

  #[test]
  fn jq_expr_object_deserializes() {
    let expr: JqExpr = serde_json::from_str(
      r#"{"__nixhapi": "jq-expr", "value": ".foo | test(\"bar\")"}"#,
    )
    .unwrap();
    assert_eq!(
      expr,
      JqExpr::Structured(JqExprStructured::Inline {
        value: r#".foo | test("bar")"#.to_string(),
      })
    );
    assert_eq!(expr.resolve().unwrap(), r#".foo | test("bar")"#);
  }

  #[test]
  fn jq_file_object_deserializes() {
    let expr: JqExpr = serde_json::from_str(
      r#"{"__nixhapi": "jq-file", "path": "/tmp/test.jq"}"#,
    )
    .unwrap();
    assert!(matches!(expr, JqExpr::Structured(JqExprStructured::File { .. })));
  }

  #[test]
  fn jq_file_read_error_is_descriptive() {
    let expr = JqExpr::Structured(JqExprStructured::File {
      path: PathBuf::from("/nonexistent/path.jq"),
    });
    let err = expr.resolve().unwrap_err();
    assert!(err.to_string().contains("/nonexistent/path.jq"));
  }
}
