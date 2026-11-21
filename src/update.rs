//! Explicit, checksum-verified updates from public GitHub release assets.
use std::fs;
use std::io::{self, Cursor, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(windows)]
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use reqwest::blocking::{Client, Response};
use reqwest::header::LOCATION;
use reqwest::redirect::Policy;
use semver::Version;
use sha2::{Digest, Sha256};
use tar::Archive;
use tempfile::NamedTempFile;
use zip::ZipArchive;

const REPO: &str = "https://github.com/codesoda/aside";
const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;
const MAX_BINARY: u64 = 128 * 1024 * 1024;
const MAX_CHECKSUMS: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: String,
    pub available: bool,
    pub tag: String,
}

fn status_for(tag: &str) -> Result<UpdateStatus> {
    let latest = tag
        .strip_prefix('v')
        .context("release tag must start with v")?;
    let latest_version = Version::parse(latest).context("invalid release version")?;
    ensure!(
        latest_version.pre.is_empty() && latest_version.build.is_empty(),
        "expected a stable release tag"
    );
    let current = env!("CARGO_PKG_VERSION");
    let available = Version::parse(current)? < latest_version;
    Ok(UpdateStatus {
        current: current.to_owned(),
        latest: latest_version.to_string(),
        available,
        tag: tag.to_owned(),
    })
}

fn tag_from_redirect(status: reqwest::StatusCode, location: Option<&str>) -> Result<&str> {
    ensure!(
        status.is_redirection(),
        "latest release returned HTTP {status}"
    );
    let location = location.context("latest release redirect has no Location header")?;
    let prefix = "/codesoda/aside/releases/tag/";
    let path = location
        .strip_prefix("https://github.com")
        .unwrap_or(location);
    let tag = path
        .strip_prefix(prefix)
        .context("unexpected release redirect")?;
    ensure!(
        !tag.is_empty() && !tag.contains(['/', '?', '#']),
        "invalid release redirect tag"
    );
    Ok(tag)
}

fn client(no_redirect: bool) -> Result<Client> {
    let redirect = if no_redirect {
        Policy::none()
    } else {
        Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 || attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        })
    };
    Ok(Client::builder()
        .redirect(redirect)
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(if no_redirect { 6 } else { 60 }))
        .user_agent(concat!("aside/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Check the public releases/latest redirect without using the GitHub API.
pub fn check() -> Result<UpdateStatus> {
    let response = client(true)?
        .get(format!("{REPO}/releases/latest"))
        .send()?;
    let location = response
        .headers()
        .get(LOCATION)
        .map(|value| value.to_str())
        .transpose()?;
    if response.status().is_redirection()
        && matches!(
            location,
            Some("https://github.com/codesoda/aside/releases" | "/codesoda/aside/releases")
        )
    {
        return Ok(UpdateStatus {
            current: env!("CARGO_PKG_VERSION").into(),
            latest: String::new(),
            available: false,
            tag: String::new(),
        });
    }
    status_for(tag_from_redirect(response.status(), location)?)
}

fn target_for(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Ok("aarch64-pc-windows-msvc"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        _ => bail!("unsupported update target: {os}/{arch}"),
    }
}

pub fn current_target() -> Result<&'static str> {
    target_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn asset_name(tag: &str, target: &str) -> String {
    let extension = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("aside-{tag}-{target}.{extension}")
}

fn download(client: &Client, url: &str, max: u64) -> Result<Vec<u8>> {
    let response: Response = client.get(url).send()?.error_for_status()?;
    if let Some(size) = response.content_length() {
        ensure!(size <= max, "download exceeds size limit: {url}");
    }
    let mut data = Vec::new();
    response.take(max + 1).read_to_end(&mut data)?;
    ensure!(
        (data.len() as u64) <= max,
        "download exceeds size limit: {url}"
    );
    Ok(data)
}

fn verify_checksum(bytes: &[u8], checksums: &str, name: &str) -> Result<()> {
    let matches: Vec<_> = checksums
        .lines()
        .filter_map(|line| {
            let (hash, file) = line.split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == name).then_some(hash)
        })
        .collect();
    ensure!(matches.len() == 1, "expected one checksum for {name}");
    let expected = matches[0];
    ensure!(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid SHA256 checksum for {name}"
    );
    ensure!(
        hex::encode(Sha256::digest(bytes)).eq_ignore_ascii_case(expected),
        "SHA256 mismatch for {name}"
    );
    Ok(())
}

fn extract_binary(bytes: &[u8], name: &str, output: &mut impl Write) -> Result<()> {
    let binary = if name.ends_with(".zip") {
        "aside.exe"
    } else {
        "aside"
    };
    if name.ends_with(".zip") {
        let mut archive = ZipArchive::new(Cursor::new(bytes))?;
        ensure!(archive.len() == 1, "archive must contain only {binary}");
        let mut entry = archive.by_index(0)?;
        ensure!(
            entry.name() == binary
                && entry.is_file()
                && entry
                    .unix_mode()
                    .is_none_or(|mode| mode & 0o170000 != 0o120000),
            "unexpected archive entry"
        );
        ensure!(entry.size() <= MAX_BINARY, "binary exceeds size limit");
        copy_bounded(&mut entry, output)?;
    } else {
        let mut archive = Archive::new(GzDecoder::new(Cursor::new(bytes)));
        let mut entries = archive.entries()?;
        let mut entry = entries.next().context("archive is empty")??;
        ensure!(
            entry.path()?.as_ref() == std::path::Path::new(binary)
                && entry.header().entry_type().is_file(),
            "unexpected archive entry"
        );
        ensure!(entry.size() <= MAX_BINARY, "binary exceeds size limit");
        copy_bounded(&mut entry, output)?;
        drop(entry);
        ensure!(entries.next().is_none(), "archive contains extra entries");
    }
    Ok(())
}

fn copy_bounded(input: &mut impl Read, output: &mut impl Write) -> Result<()> {
    let count = io::copy(&mut input.take(MAX_BINARY + 1), output)?;
    ensure!(
        count > 0 && count <= MAX_BINARY,
        "invalid replacement binary size"
    );
    Ok(())
}

/// Call only after the caller has explicitly confirmed the available update.
pub fn install(status: &UpdateStatus) -> Result<()> {
    ensure!(
        status.available && *status == status_for(&status.tag)?,
        "no valid available update to install"
    );
    let target = current_target()?;
    let name = asset_name(&status.tag, target);
    let base = format!("{REPO}/releases/download/{}", status.tag);
    let client = client(false)?;
    let checksums = download(
        &client,
        &format!("{base}/checksums-sha256.txt"),
        MAX_CHECKSUMS,
    )?;
    let checksums = std::str::from_utf8(&checksums)?;
    let archive = download(&client, &format!("{base}/{name}"), MAX_ARCHIVE)?;
    verify_checksum(&archive, checksums, &name)?;

    let exe = std::env::current_exe()?;
    let parent = exe.parent().context("executable has no parent directory")?;
    let mut staged =
        NamedTempFile::new_in(parent).context("cannot stage update next to executable")?;
    extract_binary(&archive, &name, staged.as_file_mut())?;
    staged.as_file_mut().flush()?;
    #[cfg(unix)]
    fs::set_permissions(staged.path(), fs::Permissions::from_mode(0o755))?;
    staged.as_file().sync_all()?;
    #[cfg(windows)]
    replace_with_backup(&exe, parent, || {
        self_replace::self_replace(staged.path()).context("could not replace executable")
    })?;
    #[cfg(not(windows))]
    self_replace::self_replace(staged.path()).context("could not replace executable")?;
    Ok(())
}

/// Windows self-replace can move the running executable before a later copy
/// fails. Keep a separate copy until replacement succeeds or is restored.
#[cfg(windows)]
fn replace_with_backup(
    exe: &Path,
    parent: &Path,
    replace: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let backup = NamedTempFile::new_in(parent).context("cannot stage update backup")?;
    fs::copy(exe, backup.path()).context("cannot back up the current executable")?;
    backup.as_file().sync_all()?;
    let (_, backup_path) = backup.keep().context("cannot retain update backup")?;
    if let Err(error) = replace() {
        if let Err(restore_error) = fs::copy(&backup_path, exe) {
            bail!(
                "update failed: {error}; automatic restore failed: {restore_error}; restore from {}",
                backup_path.display()
            );
        }
        let _ = fs::remove_file(&backup_path);
        return Err(error);
    }
    fs::remove_file(&backup_path).context("update installed but could not remove backup")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_and_version_status() {
        let tag = tag_from_redirect(
            reqwest::StatusCode::FOUND,
            Some("/codesoda/aside/releases/tag/v0.2.1"),
        )
        .unwrap();
        let status = status_for(tag).unwrap();
        assert_eq!((status.latest.as_str(), status.available), ("0.2.1", true));
        assert!(!status_for("v0.1.0").unwrap().available);
        assert!(!status_for("v0.0.9").unwrap().available);
        assert!(status_for("v0.2.0-beta.1").is_err());
        assert!(
            tag_from_redirect(
                reqwest::StatusCode::OK,
                Some("/codesoda/aside/releases/tag/v0.2.0")
            )
            .is_err()
        );
        assert!(
            tag_from_redirect(
                reqwest::StatusCode::FOUND,
                Some("https://evil.test/codesoda/aside/releases/tag/v0.2.0")
            )
            .is_err()
        );
    }

    #[test]
    fn assets_and_targets() {
        for (os, arch, target, extension) in [
            ("macos", "aarch64", "aarch64-apple-darwin", "tar.gz"),
            ("windows", "x86_64", "x86_64-pc-windows-msvc", "zip"),
            ("windows", "aarch64", "aarch64-pc-windows-msvc", "zip"),
            ("linux", "x86_64", "x86_64-unknown-linux-gnu", "tar.gz"),
        ] {
            assert_eq!(target_for(os, arch).unwrap(), target);
            assert_eq!(
                asset_name("v1.2.3", target),
                format!("aside-v1.2.3-{target}.{extension}")
            );
        }
        assert!(target_for("macos", "x86_64").is_err());
        assert!(target_for("linux", "aarch64").is_err());
    }

    #[test]
    fn archive_rejects_extra_entries_and_traversal() {
        let mut bytes = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
            let mut tar = tar::Builder::new(encoder);
            for name in ["aside", "extra"] {
                let mut header = tar::Header::new_gnu();
                header.set_size(4);
                header.set_mode(0o755);
                header.set_cksum();
                tar.append_data(&mut header, name, &b"data"[..]).unwrap();
            }
            tar.finish().unwrap();
        }
        assert!(
            extract_binary(
                &bytes,
                "aside-v1-x86_64-unknown-linux-gnu.tar.gz",
                &mut Vec::new()
            )
            .is_err()
        );
        let mut zip_bytes = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut zip_bytes);
            zip.start_file("../aside.exe", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"data").unwrap();
            zip.finish().unwrap();
        }
        assert!(
            extract_binary(
                zip_bytes.get_ref(),
                "aside-v1-x86_64-pc-windows-msvc.zip",
                &mut Vec::new()
            )
            .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn failed_windows_replacement_restores_original_executable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("aside.exe");
        fs::write(&exe, b"working original").unwrap();
        let failure = replace_with_backup(&exe, dir.path(), || {
            fs::remove_file(&exe)?;
            fs::write(&exe, b"incomplete replacement")?;
            Err(anyhow::anyhow!("injected copy failure"))
        });
        assert!(failure.is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"working original");
    }

    #[cfg(windows)]
    #[test]
    fn failed_windows_restore_keeps_recovery_backup() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("aside.exe");
        fs::write(&exe, b"working original").unwrap();
        let failure = replace_with_backup(&exe, dir.path(), || {
            fs::remove_file(&exe)?;
            fs::create_dir(&exe)?; // Simulate a blocked destination during rollback.
            Err(anyhow::anyhow!("injected replacement failure"))
        })
        .unwrap_err();
        assert!(failure.to_string().contains("restore from"));
        let backups: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), b"working original");
    }

    #[test]
    fn checksum_requires_exact_asset() {
        let data = b"archive";
        let name = "aside-v1.2.3-aarch64-apple-darwin.tar.gz";
        let checksums = format!("{}  {name}\n", hex::encode(Sha256::digest(data)));
        verify_checksum(data, &checksums, name).unwrap();
        assert!(verify_checksum(b"tampered", &checksums, name).is_err());
        assert!(verify_checksum(data, &checksums, "other.tar.gz").is_err());
        assert!(verify_checksum(data, &(checksums.clone() + &checksums), name).is_err());
    }
}
