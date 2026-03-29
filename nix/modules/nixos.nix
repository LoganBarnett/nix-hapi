# NixOS module for nix-hapi declarative reconciliation.
#
# Each tree produces a oneshot systemd service that pipes the evaluated
# desired state JSON directly from the Nix store and runs nix-hapi apply.
# An optional schedule produces a timer.
#
# The store path for each tree's JSON is exposed via
# config.services.nix-hapi.jsonFiles.<name> so callers can use it as a
# restartTrigger without needing to go through environment.etc.
#
# Example:
#
#   services.nix-hapi = {
#     enable = true;
#     trees.ldap-service-users = {
#       desiredState = { ... };
#       providers.ldap = "${pkgs.nix-hapi-ldap}/bin/nix-hapi-ldap";
#     };
#   };
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.nix-hapi;

  treeType = lib.types.submodule {
    options = {
      desiredState = lib.mkOption {
        type = lib.types.attrs;
        description = "Nix attrset representing the desired state tree for this provider set.";
      };

      schedule = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = ''
          Systemd calendar expression.  When null the service runs once after
          boot only.
        '';
      };

      providers = lib.mkOption {
        type = lib.types.attrsOf lib.types.str;
        default = {};
        description = ''
          Map of provider type name to binary path.  Each entry becomes a
          --provider TYPE=PATH argument passed to nix-hapi.
        '';
      };
    };
  };

  # Render --provider flags for a tree's providers map.
  providerFlags = providers:
    lib.concatStringsSep " " (
      lib.mapAttrsToList (type: path: "--provider ${type}=${path}") providers
    );

  # Generate the Nix store JSON file for a tree.
  treeJson = name: tree:
    pkgs.writeText "nix-hapi-${name}.json" (builtins.toJSON tree.desiredState);
in {
  options.services.nix-hapi = {
    enable = lib.mkEnableOption "nix-hapi declarative reconciler";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.nix-hapi or (throw "nix-hapi package not found; add nix-hapi overlay or set services.nix-hapi.package");
      description = "The nix-hapi package to use.";
    };

    trees = lib.mkOption {
      type = lib.types.attrsOf treeType;
      default = {};
      description = "Named reconciliation trees.";
    };

    jsonFiles = lib.mkOption {
      type = lib.types.attrsOf lib.types.path;
      readOnly = true;
      description = ''
        Read-only map of tree name to its desired-state JSON store path.
        Use as a restartTrigger so the service re-runs whenever the tree
        content changes.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    services.nix-hapi.jsonFiles =
      lib.mapAttrs (name: tree: treeJson name tree) cfg.trees;

    # Oneshot service per tree.
    systemd.services = lib.mapAttrs' (name: tree:
      lib.nameValuePair "nix-hapi-${name}" {
        description = "nix-hapi reconciler: ${name}";
        after = ["network-online.target"];
        wants = ["network-online.target"];
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${cfg.package}/bin/nix-hapi ${providerFlags tree.providers} apply";
          StandardInput = "file:${treeJson name tree}";
        };
        wantedBy =
          if tree.schedule == null
          then ["multi-user.target"]
          else [];
      })
    cfg.trees;

    # Optional timer per tree.
    systemd.timers = lib.mapAttrs' (name: tree:
      lib.nameValuePair "nix-hapi-${name}" {
        description = "nix-hapi timer: ${name}";
        timerConfig = {
          OnCalendar = tree.schedule;
          Persistent = true;
        };
        wantedBy = ["timers.target"];
      })
    (lib.filterAttrs (_: tree: tree.schedule != null) cfg.trees);
  };
}
