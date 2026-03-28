# Provider-agnostic nix-hapi helpers.  Flake consumers access these via
# inputs.nix-hapi.lib; local consumers import this file directly.
{
  # ── Field value constructors ──────────────────────────────────────────────────

  # Always enforce this exact value on every reconciliation.
  mkManaged = value: {
    __nixhapi = "managed";
    inherit value;
  };

  # Set once if absent; leave alone once the field is present in live state.
  mkInitial = value: {
    __nixhapi = "initial";
    inherit value;
  };

  # Never touch this field.  Documents intentional non-ownership, as opposed
  # to an accidental omission.
  mkUnmanaged = {__nixhapi = "unmanaged";};

  # Read value from a file path on every reconciliation and enforce it.
  mkManagedFromPath = path: {
    __nixhapi = "managed-from-path";
    inherit path;
  };

  # Read value from a file path; set once if absent.
  mkInitialFromPath = path: {
    __nixhapi = "initial-from-path";
    inherit path;
  };

  # Read value from an environment variable on every reconciliation and
  # enforce it.
  mkManagedFromEnv = env: {
    __nixhapi = "managed-from-env";
    inherit env;
  };

  # Read value from an environment variable; set once if absent.
  mkInitialFromEnv = env: {
    __nixhapi = "initial-from-env";
    inherit env;
  };

  # ── Dependency helpers ────────────────────────────────────────────────────────

  # Produces the jq expression used in dependsOn to declare that this provider
  # instance must not begin until the named instance has been fully applied.
  # The expression is evaluated against the complete top-level JSON blob; its
  # output is matched by equality to identify the dependency.
  #
  # Example:
  #   __nixhapi.dependsOn = [ (mkDependsOn "prod-ldap") ];
  mkDependsOn = instanceName: ".[${builtins.toJSON instanceName}]";
}
