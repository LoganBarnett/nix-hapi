mod config;
mod logging;

use clap::{Parser, Subcommand};
use config::{CliRaw, Config, ConfigError};
use logging::init_logging;
use nix_hapi_ldap::LdapProvider;
use nix_hapi_lib::meta::{NixHapiMeta, ProviderSpec};
use nix_hapi_lib::plan::{ApplyReport, Plan, ProviderPlan, ResourceChange};
use nix_hapi_lib::provider::{resolve_config, Provider, ProviderError};
use std::collections::HashMap;
use std::io::Read;
use thiserror::Error;

#[derive(Debug, Error)]
enum ApplicationError {
  #[error("Failed to load configuration during startup: {0}")]
  ConfigurationLoad(#[from] ConfigError),

  #[error("Failed to read desired state from stdin: {0}")]
  StdinRead(#[from] std::io::Error),

  #[error("Failed to parse desired state JSON: {0}")]
  JsonParse(#[from] serde_json::Error),

  #[error("Provider error for instance {instance:?}: {source}")]
  Provider {
    instance: String,
    #[source]
    source: ProviderError,
  },

  #[error("Unknown provider type {provider_type:?} for instance {instance:?}")]
  UnknownProvider {
    instance: String,
    provider_type: String,
  },

  #[error("Instance {instance:?} is missing a __nixhapi.provider declaration")]
  MissingProvider { instance: String },
}

#[derive(Debug, Parser)]
#[command(
  author,
  version,
  about = "Declarative API reconciler driven by Nix expressions"
)]
struct Cli {
  #[command(flatten)]
  raw: CliRaw,

  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Show what would change without making any API calls.
  Plan,
  /// Apply the desired state to the live APIs.
  Apply,
}

fn main() -> Result<(), ApplicationError> {
  let cli = Cli::parse();
  let config = Config::from_cli_and_file(cli.raw).map_err(|e| {
    eprintln!("Configuration error: {}", e);
    e
  })?;

  init_logging(config.log_level, config.log_format);

  let mut stdin_json = String::new();
  std::io::stdin().read_to_string(&mut stdin_json)?;
  let top_level: HashMap<String, serde_json::Value> =
    serde_json::from_str(&stdin_json)?;

  match cli.command {
    Command::Plan => run_plan(&top_level),
    Command::Apply => run_apply(&top_level),
  }
}

fn run_plan(
  top_level: &HashMap<String, serde_json::Value>,
) -> Result<(), ApplicationError> {
  let plan = build_plan(top_level)?;

  if plan.is_empty() {
    println!("No changes.");
    return Ok(());
  }

  for pp in &plan.provider_plans {
    print_provider_plan(pp);
  }

  println!();
  println!("--- Runbook (in execution order) ---");
  println!();
  for (step, instance) in plan.ordered_steps() {
    println!("[{}] {} ({})", step.order, step.description, instance);
    println!("  {}", step.command);
    if let Some(body) = &step.body {
      for line in body.lines() {
        println!("  {}", line);
      }
    }
    println!();
  }

  Ok(())
}

fn run_apply(
  top_level: &HashMap<String, serde_json::Value>,
) -> Result<(), ApplicationError> {
  let plan = build_plan(top_level)?;

  if plan.is_empty() {
    println!("No changes.");
    return Ok(());
  }

  // Apply provider plans in the order their runbook steps dictate.
  // For Phase 1 (single provider), this is straightforward.
  for pp in &plan.provider_plans {
    let (meta, _data) = split_scope(
      top_level
        .get(&pp.instance_name)
        .expect("instance must exist"),
    );
    let spec = meta.provider.as_ref().expect("provider must be set");
    let config = resolve_config(&spec.config).map_err(|source| {
      ApplicationError::Provider {
        instance: pp.instance_name.clone(),
        source,
      }
    })?;
    let provider = resolve_provider(&pp.instance_name, spec)?;

    let report = provider.apply(pp, &config).map_err(|source| {
      ApplicationError::Provider {
        instance: pp.instance_name.clone(),
        source,
      }
    })?;

    print_apply_report(&pp.instance_name, &report);
  }

  Ok(())
}

fn build_plan(
  top_level: &HashMap<String, serde_json::Value>,
) -> Result<Plan, ApplicationError> {
  let mut plan = Plan::default();

  for (instance_name, scope_value) in top_level {
    let (meta, data) = split_scope(scope_value);

    let spec = meta.provider.as_ref().ok_or_else(|| {
      ApplicationError::MissingProvider {
        instance: instance_name.clone(),
      }
    })?;

    let config = resolve_config(&spec.config).map_err(|source| {
      ApplicationError::Provider {
        instance: instance_name.clone(),
        source,
      }
    })?;

    let provider = resolve_provider(instance_name, spec)?;

    let live = provider.list_live(&config, &[]).map_err(|source| {
      ApplicationError::Provider {
        instance: instance_name.clone(),
        source,
      }
    })?;

    let mut pp =
      provider
        .plan(&data, &live, &meta, &config)
        .map_err(|source| ApplicationError::Provider {
          instance: instance_name.clone(),
          source,
        })?;

    pp.instance_name = instance_name.clone();
    plan.provider_plans.push(pp);
  }

  Ok(plan)
}

/// Splits a top-level scope value into its `__nixhapi` metadata and the
/// remaining data subtree that the provider receives.
fn split_scope(scope: &serde_json::Value) -> (NixHapiMeta, serde_json::Value) {
  let meta: NixHapiMeta = scope
    .get("__nixhapi")
    .and_then(|v| serde_json::from_value(v.clone()).ok())
    .unwrap_or_default();

  let data = match scope.as_object() {
    Some(obj) => {
      let filtered: serde_json::Map<String, serde_json::Value> = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "__nixhapi")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
      serde_json::Value::Object(filtered)
    }
    None => serde_json::Value::Null,
  };

  (meta, data)
}

/// Returns a boxed provider for the given spec, or an error for unknown types.
fn resolve_provider(
  instance: &str,
  spec: &ProviderSpec,
) -> Result<Box<dyn Provider>, ApplicationError> {
  match spec.provider_type.as_str() {
    "ldap" => Ok(Box::new(LdapProvider)),
    other => Err(ApplicationError::UnknownProvider {
      instance: instance.to_string(),
      provider_type: other.to_string(),
    }),
  }
}

fn print_provider_plan(pp: &ProviderPlan) {
  println!("=== {} ({}) ===", pp.instance_name, pp.provider_type);
  println!();

  for change in &pp.changes {
    match change {
      ResourceChange::Add {
        resource_id,
        fields,
      } => {
        println!("+ {}", resource_id);
        for f in fields {
          println!(
            "    {}: → {}",
            f.field,
            f.to.as_deref().unwrap_or("<removed>")
          );
        }
      }
      ResourceChange::Modify {
        resource_id,
        field_changes,
      } => {
        println!("~ {}", resource_id);
        for f in field_changes {
          println!(
            "    {}: {} → {}",
            f.field,
            f.from.as_deref().unwrap_or("<absent>"),
            f.to.as_deref().unwrap_or("<removed>")
          );
        }
      }
      ResourceChange::Delete { resource_id } => {
        println!("- {}", resource_id);
      }
    }
  }

  println!();
  let added = pp
    .changes
    .iter()
    .filter(|c| matches!(c, ResourceChange::Add { .. }))
    .count();
  let modified = pp
    .changes
    .iter()
    .filter(|c| matches!(c, ResourceChange::Modify { .. }))
    .count();
  let deleted = pp
    .changes
    .iter()
    .filter(|c| matches!(c, ResourceChange::Delete { .. }))
    .count();
  println!("{} to add, {} to modify, {} to delete", added, modified, deleted);
  println!();
}

fn print_apply_report(instance: &str, report: &ApplyReport) {
  println!(
    "{}: created {}, modified {}, deleted {}",
    instance,
    report.created.len(),
    report.modified.len(),
    report.deleted.len()
  );
}
