use std::{path::PathBuf, process::Command};

fn binary_path() -> PathBuf {
  let mut path =
    std::env::current_exe().expect("Could not determine test executable path");
  path.pop(); // deps/
  path.pop(); // remove deps dir
  path.push("nix-hapi");
  if !path.exists() {
    path.pop();
    path.pop();
    path.push("debug");
    path.push("nix-hapi");
  }
  path
}

#[test]
fn help_flag_exits_zero_and_mentions_usage() {
  let output = Command::new(binary_path())
    .arg("--help")
    .output()
    .expect("Failed to execute nix-hapi --help");

  assert!(
    output.status.success(),
    "Expected zero exit; got {:?}\nstderr: {}",
    output.status.code(),
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("Usage:"),
    "Expected 'Usage:' in --help output; got:\n{}",
    stdout
  );
}

#[test]
fn version_flag_exits_zero_and_mentions_binary_name() {
  let output = Command::new(binary_path())
    .arg("--version")
    .output()
    .expect("Failed to execute nix-hapi --version");

  assert!(
    output.status.success(),
    "Expected zero exit; got {:?}",
    output.status.code()
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("nix-hapi"),
    "Expected 'nix-hapi' in --version output; got:\n{}",
    stdout
  );
}

#[test]
fn plan_with_empty_desired_state_reports_no_changes() {
  let output = Command::new(binary_path())
    .arg("plan")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("Failed to spawn nix-hapi plan")
    .wait_with_output_from_stdin(b"{}")
    .expect("Failed to run nix-hapi plan");

  assert!(
    output.status.success(),
    "Expected zero exit for empty plan; stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("No changes"),
    "Expected 'No changes' for empty desired state; got:\n{}",
    stdout
  );
}

/// Helper that writes `input` to the process's stdin and waits for it to finish.
trait WaitWithInput {
  fn wait_with_output_from_stdin(
    self,
    input: &[u8],
  ) -> std::io::Result<std::process::Output>;
}

impl WaitWithInput for std::process::Child {
  fn wait_with_output_from_stdin(
    mut self,
    input: &[u8],
  ) -> std::io::Result<std::process::Output> {
    use std::io::Write;
    if let Some(ref mut stdin) = self.stdin {
      stdin.write_all(input)?;
    }
    self.wait_with_output()
  }
}
