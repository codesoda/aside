//! `aside demo` must work with no network and no `gh` (issue #17).
//!
//! Runs the real binary with:
//! - `PATH` holding only a fake `gh` that records any invocation;
//! - every proxy variable pointing at a local listener that records any
//!   outbound HTTP(S) attempt (reqwest honours these);
//! - private HOME, config and data directories.
//!
//! It then drives the demo over its loopback API (session view and the
//! synthetic refresh) and fails if `gh` ran or anything tried to leave the
//! machine through the proxy.

#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn request(url: &str, method: &str, path: &str, token: &str) -> (u16, String) {
    let authority = url
        .strip_prefix("http://")
        .unwrap()
        .trim_end_matches('/')
        .to_string();
    let mut stream = TcpStream::connect(&authority).expect("connect to demo server");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response[9..12].parse().unwrap();
    (status, response)
}

#[test]
fn demo_runs_without_network_or_gh() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let marker = root.path().join("gh-was-called");
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Any proxied network attempt lands here.
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&attempts);
    let stop = Arc::new(AtomicUsize::new(0));
    let stop_proxy = Arc::clone(&stop);
    let proxy_thread = std::thread::spawn(move || {
        while stop_proxy.load(Ordering::SeqCst) == 0 {
            if proxy.accept().is_ok() {
                seen.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });

    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_aside"));
    command
        .args(["demo", "--no-open"])
        .env_clear()
        .env("PATH", &bin)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("ASIDE_CONFIG_DIR", root.path().join("aside-config"))
        .env("ASIDE_DATA_DIR", root.path().join("aside-data"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env(var, &proxy_url);
    }
    let mut child = KillOnDrop(command.spawn().expect("spawn aside demo"));
    let stdout = child.0.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let started: serde_json::Value = loop {
        let line = lines
            .next()
            .expect("demo exited before session.started")
            .unwrap();
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value["event"] == "session.started" {
            break value;
        }
    };
    let url = started["url"].as_str().unwrap().to_string();
    let descriptor: serde_json::Value =
        serde_json::from_slice(&std::fs::read(started["descriptor"].as_str().unwrap()).unwrap())
            .unwrap();
    let token = descriptor["agentToken"].as_str().unwrap().to_string();

    let (status, body) = request(&url, "GET", "/api/v1/session", &token);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"demo\":true"));
    assert!(
        body.contains("PRRT_synthBot0001"),
        "synthetic threads are served"
    );
    let (status, body) = request(&url, "POST", "/api/v1/refresh", &token);
    assert_eq!(status, 200, "demo refresh works offline: {body}");

    std::thread::sleep(Duration::from_millis(500));
    drop(child);
    stop.store(1, Ordering::SeqCst);
    proxy_thread.join().unwrap();

    assert!(
        !marker.exists(),
        "aside demo invoked gh: {:?}",
        std::fs::read_to_string(&marker)
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "aside demo attempted an outbound network connection"
    );
}

/// The demo source must not reach for the `gh` runner, a process spawner or a
/// network client. A cheap guard that fails at review time, not runtime.
#[test]
fn demo_source_has_no_network_or_process_access() {
    for (name, source) in [
        ("src/demo.rs", include_str!("../src/demo.rs")),
        (
            "src/demo/intelligence.rs",
            include_str!("../src/demo/intelligence.rs"),
        ),
    ] {
        for banned in [
            "Command::new",
            "std::process",
            "reqwest",
            "std::net",
            "TcpStream",
            "GhRunner",
            "SystemGh",
            "crate::github",
            "update::",
            "std::fs",
        ] {
            assert!(!source.contains(banned), "{name} uses {banned}");
        }
    }
}
