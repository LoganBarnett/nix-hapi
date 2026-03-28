use nix_hapi_fake::FakeProvider;
use std::sync::{Arc, Mutex};

fn main() {
  let records = Arc::new(Mutex::new(Vec::new()));
  if let Err(e) = nix_hapi_lib::provider_host::run(FakeProvider::new(records)) {
    eprintln!("nix-hapi-fake: {e}");
    std::process::exit(1);
  }
}
