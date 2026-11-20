//! Private, per-process session descriptors for local agent clients.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{model::SCHEMA_VERSION, store, util::now_rfc3339};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub schema_version: u32,
    pub session_id: String,
    pub pid: u32,
    pub url: String,
    pub api_base: String,
    pub agent_token: String,
    pub pr: String,
    pub started_at: String,
}

pub fn directory() -> Result<PathBuf> {
    let dir = store::default_path()?
        .parent()
        .context("missing Aside data directory")?
        .join("sessions");
    fs::create_dir_all(&dir)?;
    store::restrict_dir(&dir);
    Ok(dir)
}

pub fn list() -> Result<Vec<(PathBuf, Descriptor)>> {
    let mut descriptors = Vec::new();
    for entry in fs::read_dir(directory()?)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == "json")
            && let Ok(contents) = fs::read(entry.path())
            && let Ok(descriptor) = serde_json::from_slice(&contents)
        {
            descriptors.push((entry.path(), descriptor));
        }
    }
    Ok(descriptors)
}

/// A stale PID or stale descriptor cannot make a session look active.
pub fn is_live(descriptor: &Descriptor) -> bool {
    let Some(authority) = descriptor.url.strip_prefix("http://127.0.0.1:") else {
        return false;
    };
    let Some(port) = authority.strip_suffix('/') else {
        return false;
    };
    let Ok(port) = port.parse::<u16>() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(400),
    ) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));
    let request = format!(
        "GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = Vec::new();
    if stream.take(4096).read_to_end(&mut response).is_err() {
        return false;
    }
    let Some(body_start) = response.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&response[body_start + 4..]) else {
        return false;
    };
    body.get("sessionId").and_then(|v| v.as_str()) == Some(&descriptor.session_id)
}

pub fn find_live(pr: &str) -> Result<Option<(PathBuf, Descriptor)>> {
    Ok(list()?.into_iter().find(|(_, d)| d.pr == pr && is_live(d)))
}

pub fn write(
    session_id: &str,
    url: &str,
    agent_token: &str,
    pr: &str,
) -> Result<(PathBuf, DescriptorGuard)> {
    let dir = directory()?;
    let path = dir.join(format!("{session_id}.json"));
    let descriptor = Descriptor {
        schema_version: SCHEMA_VERSION,
        session_id: session_id.into(),
        pid: std::process::id(),
        url: url.into(),
        api_base: format!("{url}api/v1"),
        agent_token: agent_token.into(),
        pr: pr.into(),
        started_at: now_rfc3339(),
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    let write_result = (|| -> Result<()> {
        serde_json::to_writer(&mut file, &descriptor)?;
        file.flush()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok((path.clone(), DescriptorGuard(path)))
}

pub struct DescriptorGuard(PathBuf);

impl Drop for DescriptorGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn status(id: &str) -> Result<(PathBuf, Descriptor, bool)> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("invalid session ID");
    }
    let path = directory()?.join(format!("{id}.json"));
    let descriptor: Descriptor = serde_json::from_slice(&fs::read(&path)?)?;
    let live = is_live(&descriptor);
    Ok((path, descriptor, live))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_rejects_path_traversal() {
        assert!(status("../secret").is_err());
        assert!(status("a/b").is_err());
    }

    #[test]
    fn liveness_does_not_contact_non_loopback_urls() {
        let descriptor = Descriptor {
            schema_version: SCHEMA_VERSION,
            session_id: "s".into(),
            pid: 1,
            url: "http://example.com:8080/".into(),
            api_base: String::new(),
            agent_token: String::new(),
            pr: String::new(),
            started_at: String::new(),
        };
        assert!(!is_live(&descriptor));
    }
}
