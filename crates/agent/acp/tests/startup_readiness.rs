#![cfg(unix)]

use cowboy_agent_acp::transport::stdio::StdioTransport;
use cowboy_agent_acp::transport::{StartupReadiness, StdioConfig};
use cowboy_agent_acp::{Client, TransportConfig};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout};

const MARKER: &str = "startup.ready+[synthetic-secret]";
const SAFETY_TIMEOUT: Duration = Duration::from_secs(5);
const PEER: &str = r#"
sleep 30 &
printf '%s\n' "$!" > "$CONTROL/descendant"
exec 3<&0
tee "$CONTROL/stdin" <&3 | while IFS= read -r request; do
    printf '%s\n' "$request" >> "$CONTROL/requests"
    if [ "$FAIL_INITIALIZE" = 1 ]; then
        printf '%s\n' '{"jsonrpc":"2.0","id":0,"error":{"code":-32000,"message":"synthetic initialization failure"}}'
    else
        printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":1,"agentCapabilities":{},"agentInfo":{"name":"local-readiness-peer","version":"1"}}}'
    fi
done &
printf '%s\n' "$$" > "$CONTROL/pid"
while [ ! -f "$CONTROL/exit" ]; do sleep 0.01; done
exit 19
"#;

struct Peer {
    root: PathBuf,
    control: PathBuf,
    config: StdioConfig,
}

impl Peer {
    fn new(base: &Path, name: &str) -> Self {
        let root = base.join("tmp");
        let control = base.join(name);
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&control).unwrap();
        let config = StdioConfig {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), PEER.into()],
            clear_env: true,
            allowed_env: vec![],
            env: vec![
                ("HOME".into(), base.to_str().unwrap().into()),
                ("CONTROL".into(), control.to_str().unwrap().into()),
            ],
            startup_readiness: Some(StartupReadiness {
                log_path: "~/tmp/session_*_{pid}/process-*.log".into(),
                contains: MARKER.into(),
                timeout_seconds: 10,
            }),
        };
        Self {
            root,
            control,
            config,
        }
    }

    async fn started<F: Future>(&self, connect: Pin<&mut F>) -> u32 {
        tokio::select! {
            _ = connect => panic!("connection completed before the controlled readiness marker"),
            _ = wait_for(|| {
                ["pid", "descendant"].iter().all(|name| {
                    fs::read_to_string(self.control.join(name))
                        .is_ok_and(|text| text.ends_with('\n') && text.trim().parse::<u32>().is_ok())
                }) && self.control.join("stdin").exists()
            }) => {}
        }
        self.pid("pid")
    }

    fn pid(&self, name: &str) -> u32 {
        fs::read_to_string(self.control.join(name))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn log(&self, pid: u32) -> PathBuf {
        self.root
            .join(format!("session_current_{pid}"))
            .join("process-current.log")
    }

    fn write_log(&self, pid: u32, text: &str) {
        let path = self.log(pid);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn assert_no_input(&self) {
        assert_eq!(
            fs::metadata(self.control.join("stdin")).unwrap().len(),
            0,
            "ACP bytes reached the peer before readiness"
        );
        assert!(!self.control.join("requests").exists());
    }

    fn assert_initialize_once(&self) {
        let requests: Vec<Value> = fs::read_to_string(self.control.join("requests"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(requests.len(), 1, "expected exactly one ACP request");
        assert_eq!(requests[0]["method"], "initialize");
        assert_eq!(requests[0]["id"], 0);
        assert_eq!(requests[0]["params"]["protocolVersion"], 1);
    }

    async fn assert_stopped(&self) {
        let pid = self.pid("pid");
        let descendant = self.pid("descendant");
        wait_for(|| !process_alive(pid) && !process_alive(descendant)).await;
    }
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    timeout(SAFETY_TIMEOUT, async {
        while !condition() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("local peer handshake or process cleanup did not complete");
}

async fn assert_pending<F: Future>(connect: Pin<&mut F>) {
    assert!(
        timeout(Duration::from_millis(250), connect).await.is_err(),
        "connection completed without its own complete readiness line"
    );
}

fn append(path: &Path, text: &str) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

fn process_alive(pid: u32) -> bool {
    if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
    }
    #[cfg(target_os = "linux")]
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat"))
        && stat
            .rsplit_once(") ")
            .is_some_and(|(_, state)| state.starts_with('Z'))
    {
        return false;
    }
    true
}

fn assert_private_error(error: &anyhow::Error) {
    assert!(
        !format!("{error:#}").contains(MARKER),
        "readiness error exposed synthetic private log content"
    );
}

#[tokio::test]
async fn client_waits_for_split_literal_complete_line_then_initializes_once() {
    let temp = tempfile::tempdir().unwrap();
    let peer = Peer::new(temp.path(), "peer");
    let mut connect = Box::pin(Client::connect(TransportConfig::Stdio(peer.config.clone())));
    let pid = peer.started(connect.as_mut()).await;
    assert!(!peer.log(pid).exists());
    assert_pending(connect.as_mut()).await;
    peer.assert_no_input();

    peer.write_log(pid, "startupXreadyyyys\n");
    assert_pending(connect.as_mut()).await;
    peer.assert_no_input();

    let (first, second) = MARKER.split_at(MARKER.len() / 2);
    append(&peer.log(pid), first);
    assert_pending(connect.as_mut()).await;
    peer.assert_no_input();

    append(&peer.log(pid), second);
    assert_pending(connect.as_mut()).await;
    peer.assert_no_input();

    append(&peer.log(pid), "\n");
    let mut client = timeout(SAFETY_TIMEOUT, connect.as_mut())
        .await
        .expect("complete readiness line was not recognized")
        .expect("initialize local peer");
    assert!(client.is_connected());
    assert_eq!(
        client.agent_info.as_ref().unwrap().name,
        "local-readiness-peer"
    );
    peer.assert_initialize_once();
    client.close().await.unwrap();
    peer.assert_stopped().await;
    peer.assert_initialize_once();
}

#[tokio::test]
async fn ignores_preexisting_and_wrong_pid_logs_until_new_matching_log_is_created() {
    let temp = tempfile::tempdir().unwrap();
    let peer = Peer::new(temp.path(), "peer");
    let stale = peer
        .root
        .join(format!("session_stale_{}", std::process::id()));
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("process-stale.log"), format!("{MARKER}\n")).unwrap();
    let mut connect = Box::pin(Client::connect(TransportConfig::Stdio(peer.config.clone())));
    let pid = peer.started(connect.as_mut()).await;

    peer.write_log(pid + 1, &format!("{MARKER}\n"));
    let own_directory = peer.log(pid).parent().unwrap().to_path_buf();
    fs::create_dir_all(&own_directory).unwrap();
    fs::write(own_directory.join("unrelated.txt"), format!("{MARKER}\n")).unwrap();
    assert_pending(connect.as_mut()).await;
    peer.assert_no_input();

    peer.write_log(pid, "still starting\n");
    assert_pending(connect.as_mut()).await;
    peer.assert_no_input();

    fs::write(
        own_directory.join("process-later.log"),
        format!("{MARKER}\n"),
    )
    .unwrap();
    let mut client = timeout(SAFETY_TIMEOUT, connect.as_mut())
        .await
        .expect("new matching log was not discovered")
        .expect("initialize local peer");
    peer.assert_initialize_once();
    client.close().await.unwrap();
    peer.assert_stopped().await;
}

#[tokio::test]
async fn early_exit_before_marker_never_initializes_and_cleans_descendants() {
    let temp = tempfile::tempdir().unwrap();
    let peer = Peer::new(temp.path(), "peer");
    let mut connect = Box::pin(Client::connect(TransportConfig::Stdio(peer.config.clone())));
    let pid = peer.started(connect.as_mut()).await;
    peer.write_log(pid, "synthetic private log prelude\n");
    peer.assert_no_input();
    fs::write(peer.control.join("exit"), "exit now").unwrap();

    let error = timeout(SAFETY_TIMEOUT, connect.as_mut())
        .await
        .expect("early exit was mistaken for a readiness wait")
        .expect_err("exited peer must not connect");
    assert_private_error(&error);
    assert!(!format!("{error:#}").contains("synthetic private log prelude"));
    peer.assert_stopped().await;
    peer.assert_no_input();
}

#[tokio::test]
async fn one_second_timeout_ignores_unterminated_marker_and_cleans_descendants() {
    let temp = tempfile::tempdir().unwrap();
    let mut peer = Peer::new(temp.path(), "peer");
    peer.config
        .startup_readiness
        .as_mut()
        .unwrap()
        .timeout_seconds = 1;
    let started = Instant::now();
    let mut connect = Box::pin(Client::connect(TransportConfig::Stdio(peer.config.clone())));
    let pid = peer.started(connect.as_mut()).await;
    peer.write_log(pid, MARKER);

    let error = timeout(SAFETY_TIMEOUT, connect.as_mut())
        .await
        .expect("one-second readiness timeout did not bound connection")
        .expect_err("unterminated readiness marker must time out");
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_private_error(&error);
    peer.assert_stopped().await;
    peer.assert_no_input();
}

#[tokio::test]
async fn initialize_failure_after_readiness_cleans_owned_descendants() {
    let temp = tempfile::tempdir().unwrap();
    let mut peer = Peer::new(temp.path(), "peer");
    peer.config.env.push(("FAIL_INITIALIZE".into(), "1".into()));
    let mut connect = Box::pin(Client::connect(TransportConfig::Stdio(peer.config.clone())));
    let pid = peer.started(connect.as_mut()).await;
    peer.assert_no_input();
    peer.write_log(pid, &format!("{MARKER}\n"));

    let error = timeout(SAFETY_TIMEOUT, connect.as_mut())
        .await
        .expect("initialize failure did not complete")
        .expect_err("synthetic initialize failure must reject connection");
    assert_private_error(&error);
    peer.assert_initialize_once();
    peer.assert_stopped().await;
}

#[tokio::test]
async fn cancelling_startup_and_dropping_ready_transport_kill_owned_descendants() {
    let temp = tempfile::tempdir().unwrap();
    for (name, ready) in [("cancelled", false), ("ready", true)] {
        let peer = Peer::new(temp.path(), name);
        let mut connect = Box::pin(StdioTransport::connect(&peer.config, &[]));
        let pid = peer.started(connect.as_mut()).await;
        peer.assert_no_input();
        if ready {
            peer.write_log(pid, &format!("{MARKER}\n"));
            let transport = timeout(SAFETY_TIMEOUT, connect.as_mut())
                .await
                .expect("ready transport did not connect")
                .expect("connect local transport");
            drop(connect);
            peer.assert_no_input();
            drop(transport);
        } else {
            assert_pending(connect.as_mut()).await;
            drop(connect);
        }
        peer.assert_stopped().await;
        peer.assert_no_input();
    }
}

#[tokio::test]
async fn concurrent_clients_with_shared_log_root_cannot_cross_satisfy_readiness() {
    let temp = tempfile::tempdir().unwrap();
    let first = Peer::new(temp.path(), "first");
    let second = Peer::new(temp.path(), "second");
    let mut first_connect = Box::pin(Client::connect(TransportConfig::Stdio(
        first.config.clone(),
    )));
    let mut second_connect = Box::pin(Client::connect(TransportConfig::Stdio(
        second.config.clone(),
    )));
    let (first_pid, second_pid) = tokio::join!(
        first.started(first_connect.as_mut()),
        second.started(second_connect.as_mut()),
    );
    assert_ne!(first_pid, second_pid);
    first.write_log(first_pid, &format!("{MARKER}\n"));

    let (first_result, ()) = tokio::join!(
        timeout(SAFETY_TIMEOUT, first_connect.as_mut()),
        assert_pending(second_connect.as_mut()),
    );
    let mut first_client = first_result
        .expect("first peer did not recognize its own marker")
        .expect("initialize first local peer");
    first.assert_initialize_once();
    second.assert_no_input();

    second.write_log(second_pid, &format!("{MARKER}\n"));
    let mut second_client = timeout(SAFETY_TIMEOUT, second_connect.as_mut())
        .await
        .expect("second peer did not recognize its own marker")
        .expect("initialize second local peer");
    second.assert_initialize_once();
    first_client.close().await.unwrap();
    second_client.close().await.unwrap();
    first.assert_stopped().await;
    second.assert_stopped().await;
}
