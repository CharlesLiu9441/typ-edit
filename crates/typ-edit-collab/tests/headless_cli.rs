use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use typ_edit_collab::{ClientEvent, CollabClient};

fn temp_root() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "typ-edit-headless-cli-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create root");
    root
}

#[test]
fn headless_server_cli_serves_a_snapshot() {
    let root = temp_root();
    std::fs::write(root.join("main.typ"), "from the headless server").expect("write main");

    let mut child = Command::new(env!("CARGO_BIN_EXE_typ-edit-collab-server"))
        .arg("--root")
        .arg(&root)
        .args([
            "--port",
            "0",
            "--no-advertise",
            "--user",
            "srv",
            "--project",
            "proj",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start headless server");
    let stdout = child.stdout.take().expect("stdout");
    let (port_tx, port_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        for line in lines.by_ref().map_while(Result::ok) {
            if let Some(port) = line
                .strip_prefix("HEADLESS_SERVER_READY port=")
                .and_then(|value| value.parse::<u16>().ok())
            {
                let _ = port_tx.send(port);
                return;
            }
        }
    });

    let port = port_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the CLI must print its ready port");
    let address: SocketAddr = ([127, 0, 0, 1], port).into();
    let client = CollabClient::connect(&[address], "ann", None).expect("connect to the CLI server");
    client.set_open_files(vec!["main.typ".to_owned()]);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut text = None;
    let mut complete = false;
    while Instant::now() < deadline && !complete {
        while let Some(event) = client.try_recv_event() {
            match event {
                ClientEvent::FileText { path, text: body } if path == "main.typ" => {
                    text = Some(body);
                }
                ClientEvent::SnapshotComplete => complete = true,
                _ => {}
            }
        }
        if !complete {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(complete, "the headless server must send SnapshotDone");
    assert_eq!(text.as_deref(), Some("from the headless server"));
    client.ready();

    drop(client);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);
}
