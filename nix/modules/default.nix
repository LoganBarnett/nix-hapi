# NixOS and nix-darwin module for nix-hapi declarative reconciliation.
#
# Each tree produces a oneshot systemd service (or launchd daemon on Darwin)
# that reads the evaluated desired state from /etc/nix-hapi/<name>.json and
# runs nix-hapi apply.  An optional schedule produces a timer.
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
          Systemd calendar expression (NixOS) or launchd StartCalendarInterval
          (nix-darwin).  When null the service runs once after boot only.
        '';
      };

      providers = lib.mkOption {
        type = lib.types.attrsOf lib.types.str;
        default = {};
        description = ''
          Map of provider type name to binary path.  Each entry becomes a
          --provider TYPE=PATH argument to nix-hapi apply.
        '';
      };
    };
  };

  # Render --provider flags for a tree's providers map.
  providerFlags = providers:
    lib.concatStringsSep " " (
      lib.mapAttrsToList (type: path: "--provider ${type}=${path}") providers
    );

  # Generate the JSON file for a tree.
  treeJson = name: tree:
    pkgs.writeText "nix-hapi-${name}.json" (builtins.toJSON tree.desiredState);

  # Check whether we are on a Darwin host.
  isDarwin = pkgs.stdenv.isDarwin;
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
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      # ── NixOS (systemd) ──────────────────────────────────────────────────
      (lib.mkIf (!isDarwin) {
        # Write the JSON desired state for each tree.
        environment.etc = lib.mapAttrs' (name: tree:
          lib.nameValuePair "nix-hapi/${name}.json" {
            source = treeJson name tree;
          })
        cfg.trees;

        # Oneshot service per tree.
        systemd.services = lib.mapAttrs' (name: tree:
          lib.nameValuePair "nix-hapi-${name}" {
            description = "nix-hapi reconciler: ${name}";
            after = ["network-online.target"];
            wants = ["network-online.target"];
            serviceConfig = {
              Type = "oneshot";
              ExecStart = "${cfg.package}/bin/nix-hapi apply ${providerFlags tree.providers} < /etc/nix-hapi/${name}.json";
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
      })

      # ── nix-darwin (launchd) ─────────────────────────────────────────────
      (lib.mkIf isDarwin {
        # Write the JSON desired state for each tree.
        environment.etc = lib.mapAttrs' (name: tree:
          lib.nameValuePair "nix-hapi/${name}.json" {
            source = treeJson name tree;
          })
        cfg.trees;

        launchd.daemons = lib.mapAttrs' (name: tree:
          lib.nameValuePair "nix-hapi-${name}" {
            serviceConfig =
              {
                Label = "com.nix-hapi.${name}";
                ProgramArguments = [
                  "/bin/sh"
                  "-c"
                  "${cfg.package}/bin/nix-hapi apply ${providerFlags tree.providers} < /etc/nix-hapi/${name}.json"
                ];
                RunAtLoad = tree.schedule == null;
              }
              // (
                if tree.schedule != null
                then {
                  serviceConfig.StartCalendarInterval = tree.schedule;
                }
                else {}
              );
          })
        cfg.trees;
      })
    ]
  );
}
