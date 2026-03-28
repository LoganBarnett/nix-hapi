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

  # ── Path proxy ────────────────────────────────────────────────────────────────

  # Builds a path-aware proxy that mirrors `config`.  Every node in the proxy
  # carries a `path` attribute containing its absolute jq address from the
  # top-level JSON root, ready for use as a derivedFrom input.
  #
  # Recursion stops at FieldValue leaves — attrsets whose __nixhapi key is a
  # string discriminant (e.g. "managed", "derived-from").  Plain data objects
  # and provider meta blocks (where __nixhapi is itself an attrset) are
  # traversed normally.
  #
  # Example:
  #   let
  #     config = { "hr-system" = ldap.mkLdapProvider { ... }; };
  #     tree   = lib.mkTree config;
  #   in
  #     tree."hr-system".users.alice.uid.path
  #     # => ".[\"hr-system\"][\"users\"][\"alice\"][\"uid\"]"
  mkTree = let
    buildNode = prefix: node:
      if
        builtins.isAttrs node
        && !(builtins.isString (node.__nixhapi or null))
      then
        builtins.mapAttrs
        (k: v: buildNode "${prefix}[${builtins.toJSON k}]" v)
        node
        // {path = prefix;}
      else {path = prefix;};
  in
    config:
      builtins.mapAttrs
      (instance: scope:
        buildNode ".[${builtins.toJSON instance}]" scope)
      config;

  # ── derivedFrom ───────────────────────────────────────────────────────────────

  # Declares a field whose value is computed at reconciliation time from live
  # state produced by earlier waves.
  #
  # `inputs` maps short aliases to absolute jq paths obtained from mkTree.
  # `expression` is a jq program evaluated with `.` bound to the resolved
  # inputs object `{ alias: value, ... }`.  The mk* helpers (mkManaged,
  # mkInitial, etc.) are available in the expression without any preamble.
  #
  # Each input creates an implicit DAG edge: the owning instance cannot begin
  # until the instance named in the path has been fully applied.
  #
  # Example:
  #   let tree = lib.mkTree config; in
  #   userId = lib.mkDerivedFrom {
  #     inputs     = { uid = tree."hr-system".users.alice.uid.path; };
  #     expression = "mkManaged(.uid)";
  #   };
  mkDerivedFrom = {
    inputs,
    expression,
  }: {
    __nixhapi = "derived-from";
    inherit inputs expression;
  };
}
