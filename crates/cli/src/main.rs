mod config;
mod logging;

use clap::{Parser, Subcommand};
use config::{CliRaw, Config, ConfigError};
use logging::init_logging;
use nix_hapi_lib::executor::{
  execute_apply_waves, execute_plan_waves, ExecuteError,
};
use nix_hapi_lib::plan::{ApplyReport, ProviderPlan, ResourceChange};
use nix_hapi_lib::provider::Provider;
use nix_hapi_lib::subprocess::SubprocessProvider;
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

  #[error("{0}")]
  Execute(#[from] ExecuteError),
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

  let resolver = |instance: &str, provider_type: &str| {
    let path = config.providers.get(provider_type).ok_or_else(|| {
      ExecuteError::ProviderLookup {
        instance: instance.to_string(),
        provider_type: provider_type.to_string(),
      }
    })?;
    SubprocessProvider::spawn(provider_type.to_string(), path)
      .map_err(|e| ExecuteError::ProviderInit {
        instance: instance.to_string(),
        message: e.to_string(),
      })
      .map(|p| Box::new(p) as Box<dyn Provider>)
  };

  match cli.command {
    Command::Plan => run_plan(&top_level, resolver),
    Command::Apply => run_apply(&top_level, resolver),
  }
}

fn run_plan<F>(
  top_level: &HashMap<String, serde_json::Value>,
  resolver: F,
) -> Result<(), ApplicationError>
where
  F: Fn(&str, &str) -> Result<Box<dyn Provider>, ExecuteError> + Sync,
{
  let plan = execute_plan_waves(top_level, resolver)?;

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
    println!("{} ({})", step.description, instance);
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

fn run_apply<F>(
  top_level: &HashMap<String, serde_json::Value>,
  resolver: F,
) -> Result<(), ApplicationError>
where
  F: Fn(&str, &str) -> Result<Box<dyn Provider>, ExecuteError> + Sync,
{
  let reports = execute_apply_waves(top_level, resolver)?;

  let any_changes = reports.iter().any(|(_, r)| {
    !r.created.is_empty() || !r.modified.is_empty() || !r.deleted.is_empty()
  });

  if !any_changes {
    println!("No changes.");
    return Ok(());
  }

  for (instance, report) in &reports {
    print_apply_report(instance, report);
  }

  Ok(())
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
