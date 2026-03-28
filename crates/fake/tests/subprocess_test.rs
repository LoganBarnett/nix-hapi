use nix_hapi_lib::meta::NixHapiMeta;
use nix_hapi_lib::plan::ResourceChange;
use nix_hapi_lib::provider::Provider;
use nix_hapi_lib::subprocess::SubprocessProvider;
use std::collections::HashMap;
use std::path::Path;

#[test]
fn subprocess_smoke() {
  let binary = env!("CARGO_BIN_EXE_nix-hapi-fake");
  let provider =
    SubprocessProvider::spawn("fake".to_string(), Path::new(binary))
      .expect("failed to spawn nix-hapi-fake");

  let config = HashMap::new();
  let filters = [];

  let live = provider
    .list_live(&config, &filters)
    .expect("list_live failed");
  assert_eq!(live, serde_json::json!({}));

  let desired = serde_json::json!({});
  let meta = NixHapiMeta::default();
  let plan = provider
    .plan(&desired, &live, &meta, &config)
    .expect("plan failed");
  assert_eq!(plan.changes.len(), 1);
  assert!(matches!(plan.changes[0], ResourceChange::Add { .. }));

  let report = provider.apply(&plan, &config).expect("apply failed");
  assert_eq!(report.created, vec!["fake-resource".to_string()]);
}
