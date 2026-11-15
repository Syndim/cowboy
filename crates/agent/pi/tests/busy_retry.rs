//! Regression test for the Linux ETXTBSY flake: spawning a script while it
//! is still open for writing must succeed via the bounded retry.

use cowboy_agent_pi::process::{PiProcess, PiProcessConfig};
use std::fs::OpenOptions;
use std::time::Duration;

#[cfg(unix)]
#[tokio::test]
async fn spawn_succeeds_while_script_open_for_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("busy-stub.sh");
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    // Hold the script open for writing exactly like a concurrent writer
    // would during the ETXTBSY window.
    let writer = OpenOptions::new().append(true).open(&path).unwrap();

    let config = PiProcessConfig {
        command: path.to_string_lossy().to_string(),
        args: vec![],
        clear_env: false,
        allowed_env: vec![],
        env: vec![],
    };
    // Without the retry this fails immediately on Linux with
    // "Text file busy"; with it, the spawn succeeds once the writer
    // releases the file shortly after.
    let handle = tokio::spawn(async move {
        std::thread::sleep(Duration::from_millis(50));
        drop(writer);
    });

    let process = PiProcess::spawn(&config).await;
    handle.await.unwrap();
    let mut process = process.expect("busy retry must defeat ETXTBSY");
    let _ = process.wait_for_exit(Duration::from_secs(5)).await;
}
