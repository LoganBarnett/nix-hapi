use clap::Parser;
use nix_hapi_lib::{LogFormat, LogLevel};
use serde::Deserialize;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
  #[error(
    "Failed to read configuration file at {path:?} during startup: {source}"
  )]
  FileRead {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },

  #[error("Failed to parse configuration file at {path:?}: {source}")]
  Parse {
    path: PathBuf,
    #[source]
    source: toml::de::Error,
  },

  #[error("Configuration validation failed: {0}")]
  Validation(String),
}

#[derive(Debug, Parser)]
#[command(
  author,
  version,
  about = "Declarative API reconciler driven by Nix expressions"
)]
pub struct CliRaw {
  /// Log level (trace, debug, info, warn, error).
  #[arg(long, env = "NIX_HAPI_LOG_LEVEL")]
  pub log_level: Option<String>,

  /// Log format (text, json).
  #[arg(long, env = "NIX_HAPI_LOG_FORMAT")]
  pub log_format: Option<String>,

  /// Path to the nix-hapi configuration file (TOML).
  #[arg(short, long, env = "NIX_HAPI_CONFIG")]
  pub config: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ConfigFileRaw {
  pub log_level: Option<String>,
  pub log_format: Option<String>,
}

impl ConfigFileRaw {
  pub fn from_file(path: &PathBuf) -> Result<Self, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|source| {
      ConfigError::FileRead {
        path: path.clone(),
        source,
      }
    })?;
    toml::from_str(&contents).map_err(|source| ConfigError::Parse {
      path: path.clone(),
      source,
    })
  }
}

#[derive(Debug)]
pub struct Config {
  pub log_level: LogLevel,
  pub log_format: LogFormat,
}

impl Config {
  pub fn from_cli_and_file(cli: CliRaw) -> Result<Self, ConfigError> {
    let config_file = if let Some(ref path) = cli.config {
      ConfigFileRaw::from_file(path)?
    } else {
      let default = PathBuf::from("config.toml");
      if default.exists() {
        ConfigFileRaw::from_file(&default)?
      } else {
        ConfigFileRaw::default()
      }
    };

    let log_level = cli
      .log_level
      .or(config_file.log_level)
      .unwrap_or_else(|| "info".to_string())
      .parse::<LogLevel>()
      .map_err(|e| ConfigError::Validation(e.to_string()))?;

    let log_format = cli
      .log_format
      .or(config_file.log_format)
      .unwrap_or_else(|| "text".to_string())
      .parse::<LogFormat>()
      .map_err(|e| ConfigError::Validation(e.to_string()))?;

    Ok(Config {
      log_level,
      log_format,
    })
  }
}
