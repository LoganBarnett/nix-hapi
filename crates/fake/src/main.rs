use nix_hapi_fake::FakeProvider;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
  let records = Arc::new(Mutex::new(Vec::new()));
  if let Err(e) =
    nix_hapi_lib::provider_host::run(FakeProvider::new(records)).await
  {
    eprintln!("nix-hapi-fake: {e}");
    std::process::exit(1);
  }
}
