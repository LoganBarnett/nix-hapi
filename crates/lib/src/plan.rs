/// The complete plan for one provider instance.
#[derive(Debug)]
pub struct ProviderPlan {
  pub instance_name: String,
  pub provider_type: String,
  pub changes: Vec<ResourceChange>,
  pub runbook: Vec<RunbookStep>,
}

impl ProviderPlan {
  pub fn is_empty(&self) -> bool {
    self.changes.is_empty()
  }
}

/// A change to a single resource.
#[derive(Debug)]
pub enum ResourceChange {
  Add {
    resource_id: String,
    fields: Vec<FieldDiff>,
  },
  Modify {
    resource_id: String,
    field_changes: Vec<FieldDiff>,
  },
  Delete {
    resource_id: String,
  },
}

impl ResourceChange {
  pub fn resource_id(&self) -> &str {
    match self {
      ResourceChange::Add { resource_id, .. }
      | ResourceChange::Modify { resource_id, .. }
      | ResourceChange::Delete { resource_id } => resource_id.as_str(),
    }
  }
}

/// A change to a single field within a resource.
#[derive(Debug)]
pub struct FieldDiff {
  pub field: String,
  /// Current live value; `None` if the field does not exist yet.
  pub from: Option<String>,
  /// Desired value; `None` if the field is being removed.
  pub to: Option<String>,
}

/// A single step in the execution runbook.
#[derive(Debug)]
pub struct RunbookStep {
  /// Execution order.  Steps with lower order run before steps with higher
  /// order.  Steps with equal order may eventually run concurrently.
  pub order: u32,

  /// Human-readable label shown in plan output.
  pub description: String,

  /// The command or request line shown to the operator, with all sensitive
  /// values replaced by `***`.
  pub command: String,

  /// Body or payload (e.g. an LDIF block) shown in plan output, if any.
  pub body: Option<String>,

  /// Machine-executable representation of the operation, opaque to the core.
  /// The provider serialises its own operation type here and deserialises it
  /// again during `apply`.
  pub operation: serde_json::Value,
}

/// Summary of changes applied by a provider.
#[derive(Debug, Default)]
pub struct ApplyReport {
  pub created: Vec<String>,
  pub modified: Vec<String>,
  pub deleted: Vec<String>,
}

/// The unified plan across all provider instances.
#[derive(Debug, Default)]
pub struct Plan {
  pub provider_plans: Vec<ProviderPlan>,
}

impl Plan {
  /// All runbook steps across all providers, in execution order.
  pub fn ordered_steps(&self) -> Vec<(&RunbookStep, &str)> {
    let mut steps: Vec<(&RunbookStep, &str)> = self
      .provider_plans
      .iter()
      .flat_map(|pp| {
        pp.runbook
          .iter()
          .map(move |step| (step, pp.instance_name.as_str()))
      })
      .collect();
    steps.sort_by_key(|(step, _)| step.order);
    steps
  }

  pub fn is_empty(&self) -> bool {
    self.provider_plans.iter().all(|pp| pp.is_empty())
  }
}
