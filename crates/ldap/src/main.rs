use nix_hapi_ldap::LdapProvider;

fn main() {
  if let Err(e) = nix_hapi_lib::provider_host::run(LdapProvider) {
    eprintln!("nix-hapi-ldap: {e}");
    std::process::exit(1);
  }
}
