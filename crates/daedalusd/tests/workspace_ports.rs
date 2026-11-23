use daedalusd::runtime_monitor::discover_local_ports_for_roots;
use std::io::Read;
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ROLE: &str = "DAEDALUS_PORT_TEST_ROLE";
const DIRECTORY: &str = "DAEDALUS_PORT_TEST_DIRECTORY";

fn child_command(role: &str, dir: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "workspace_ports", "--nocapture"])
        .env(ROLE, role)
        .env(DIRECTORY, dir)
        .env("PATH", "")
        .stdout(Stdio::null());
    command
}

struct Workspace(Child);

impl Drop for Workspace {
    fn drop(&mut self) {
        // Closing this pipe also releases the grandchild through its inherited stdin.
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

fn start_workspace(dir: &Path, exclude_port: u16) -> (Workspace, u16) {
    std::fs::create_dir(dir).unwrap();
    let workspace = Workspace(
        child_command("root", dir)
            .env("DAEDALUS_PORT_TEST_EXCLUDE", exclude_port.to_string())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let ports = loop {
        if let Ok(ports) = std::fs::read_to_string(dir.join("ready")) {
            break ports;
        }
        assert!(
            Instant::now() < deadline,
            "workspace listener never became ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    (workspace, ports.parse().unwrap())
}

#[test]
fn workspace_ports() {
    if let Ok(role) = std::env::var(ROLE) {
        let dir = PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
        match role.as_str() {
            "root" => assert!(child_command("listener", &dir).status().unwrap().success()),
            "listener" => {
                let address = if dir.ends_with("second") {
                    "[::1]:0"
                } else {
                    "127.0.0.1:0"
                };
                let excluded: u16 = std::env::var("DAEDALUS_PORT_TEST_EXCLUDE")
                    .unwrap()
                    .parse()
                    .unwrap();
                let listener = loop {
                    let socket = TcpListener::bind(address).unwrap();
                    if socket.local_addr().unwrap().port() != excluded {
                        break socket;
                    }
                };
                let _udp = UdpSocket::bind("127.0.0.1:0").unwrap();
                let tcp_port = listener.local_addr().unwrap().port();
                std::fs::write(dir.join("pending"), tcp_port.to_string()).unwrap();
                std::fs::rename(dir.join("pending"), dir.join("ready")).unwrap();
                std::io::stdin().read_exact(&mut [0]).unwrap_err();
            }
            "probe" => {
                let (first, a) = start_workspace(&dir.join("first"), 0);
                let (second, b) = start_workspace(&dir.join("second"), a);
                for (pid, wanted, sibling) in [(first.0.id(), a, b), (second.0.id(), b, a)] {
                    let ports = discover_local_ports_for_roots(&[pid]);
                    assert!(
                        ports.iter().any(|p| p.host_port == wanted),
                        "{ports:?} missed {wanted}"
                    );
                    assert!(
                        !ports.iter().any(|p| p.host_port == sibling),
                        "{ports:?} included sibling {sibling}"
                    );
                    assert_eq!(ports.len(), 1, "unexpected listening ports: {ports:?}");
                }
            }
            _ => panic!("unknown fixture role"),
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    assert!(
        child_command("probe", dir.path())
            .status()
            .unwrap()
            .success()
    );
}
