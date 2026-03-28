# LDAP provider helpers.  Accepts nixHapiLib so field value constructors are
# shared without duplication.
#
# Typical usage as a flake consumer:
#
#   let
#     lib  = inputs.nix-hapi.lib;
#     ldap = lib.ldap;
#   in {
#     prod-ldap = ldap.mkLdapProvider {
#       url          = lib.mkManaged "ldap://ldap.example.org";
#       baseDn       = lib.mkManaged "dc=example,dc=org";
#       bindDn       = lib.mkManaged "cn=admin,dc=example,dc=org";
#       bindPassword = lib.mkManagedFromPath "/run/secrets/ldap-admin-password";
#       users = {
#         alice = ldap.mkLdapUser {
#           cn           = lib.mkManaged "Alice Smith";
#           sn           = lib.mkManaged "Smith";
#           mail         = lib.mkManaged "alice@example.org";
#           userPassword = lib.mkInitialFromPath "/run/secrets/alice-password";
#         };
#       };
#     };
#   }
{nixHapiLib}: {
  # Builds a complete LDAP provider scope for use as a top-level provider
  # instance.  url, baseDn, bindDn, and bindPassword must be FieldValue
  # attrsets (mkManaged, mkManagedFromPath, etc.) — not bare strings — so the
  # reconciler knows how to resolve and manage each credential.
  mkLdapProvider = {
    url,
    baseDn,
    bindDn,
    bindPassword,
    ignore ? [],
    dependsOn ? [],
    users ? {},
    groups ? {},
  }: {
    __nixhapi = {
      provider = {
        type = "ldap";
        inherit url baseDn bindDn bindPassword;
      };
      inherit ignore dependsOn;
    };
    inherit users groups;
  };

  # Builds an LDAP user entry.  cn, sn, mail, and userPassword are required
  # by inetOrgPerson; all must be FieldValue attrsets.  Additional LDAP
  # attributes (e.g. loginShell, homeDirectory) may be passed via extraFields
  # using the same FieldValue encoding.
  mkLdapUser = {
    cn,
    sn,
    mail,
    userPassword,
    extraFields ? {},
  }:
    {inherit cn sn mail userPassword;}
    // extraFields;
}
