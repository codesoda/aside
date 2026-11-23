//! Install the skill embedded in the binary without a second network download.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

const CONTENT: &str = include_str!("../skills/aside/SKILL.md");
const MARKER: &str = ".aside-managed";

fn marker_for(content: &str) -> String {
    format!(
        "aside skill sha256:{}\n",
        hex::encode(Sha256::digest(content.as_bytes()))
    )
}

fn is_managed(target: &Path) -> Result<bool> {
    let content = match fs::read_to_string(target.join("SKILL.md")) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    if fs::read_to_string(target.join(MARKER)).ok() != Some(marker_for(&content)) {
        return Ok(false);
    }
    for entry in fs::read_dir(target)? {
        let name = entry?.file_name();
        if name != "SKILL.md" && name != MARKER {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn selected(claude: bool, codex: bool, agents: bool) -> Vec<&'static str> {
    if !claude && !codex && !agents {
        return vec![".claude", ".codex", ".agents"];
    }
    let mut roots = Vec::new();
    if claude {
        roots.push(".claude");
    }
    if codex {
        roots.push(".codex");
    }
    if agents {
        roots.push(".agents");
    }
    roots
}

/// Only replace a skill directory that a previous invocation created.
pub fn install(home: &Path, roots: &[&str]) -> Result<Vec<PathBuf>> {
    let mut installed = Vec::new();
    for root in roots {
        let skills = home.join(root).join("skills");
        fs::create_dir_all(&skills)
            .with_context(|| format!("cannot create {}", skills.display()))?;
        let target = skills.join("aside");
        let previous = match fs::symlink_metadata(&target) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() || !is_managed(&target)?
                {
                    bail!(
                        "{} already exists and is not managed by Aside; leave it unchanged",
                        target.display()
                    );
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", target.display()));
            }
        };

        // Stage beside the destination so the final rename stays on the same filesystem.
        let staging = tempfile::tempdir_in(&skills)?;
        let next = staging.path().join("aside");
        fs::create_dir(&next)?;
        fs::write(next.join("SKILL.md"), CONTENT)?;
        fs::write(next.join(MARKER), marker_for(CONTENT))?;
        if previous {
            let backup = staging.path().join("previous");
            fs::rename(&target, &backup)
                .with_context(|| format!("cannot replace {}", target.display()))?;
            if let Err(error) = fs::rename(&next, &target) {
                fs::rename(&backup, &target).context("failed to restore previous skill")?;
                return Err(error).with_context(|| format!("cannot install {}", target.display()));
            }
        } else {
            fs::rename(&next, &target)
                .with_context(|| format!("cannot install {}", target.display()))?;
        }
        installed.push(target);
    }
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_all_or_uses_only_selected_agents() {
        assert_eq!(
            selected(false, false, false),
            [".claude", ".codex", ".agents"]
        );
        assert_eq!(selected(true, false, true), [".claude", ".agents"]);
    }

    #[test]
    fn installs_and_replaces_only_its_own_skill() {
        let home = tempfile::tempdir().unwrap();
        let roots = selected(false, false, false);
        let paths = install(home.path(), &roots).unwrap();
        assert_eq!(paths.len(), 3);
        for path in &paths {
            assert_eq!(fs::read_to_string(path.join("SKILL.md")).unwrap(), CONTENT);
            assert_eq!(
                fs::read_to_string(path.join(MARKER)).unwrap(),
                marker_for(CONTENT)
            );
        }
        fs::write(paths[0].join("SKILL.md"), "old version").unwrap();
        fs::write(paths[0].join(MARKER), marker_for("old version")).unwrap();
        install(home.path(), &[".claude"]).unwrap();
        assert_eq!(
            fs::read_to_string(paths[0].join("SKILL.md")).unwrap(),
            CONTENT
        );
        assert!(!paths[0].join("previous").exists());
    }

    #[test]
    fn embedded_skill_uses_native_triage_and_requires_external_model_consent() {
        assert!(CONTENT.contains("use Aside's native analysis as leads, not verdicts"));
        assert!(
            CONTENT
                .contains("Do not invoke an external model-backed tool without explicit consent")
        );
        let instructions = CONTENT.to_ascii_lowercase();
        assert!(!instructions.contains("`inspect"));
        for command in ["inspect pr", "inspect diff", "inspect review"] {
            assert!(!instructions.contains(command), "unexpected `{command}`");
        }
    }

    #[test]
    fn refuses_to_delete_user_files_on_update() {
        let home = tempfile::tempdir().unwrap();
        let path = install(home.path(), &[".claude"]).unwrap().remove(0);
        fs::write(path.join("notes.md"), "keep me").unwrap();
        assert!(install(home.path(), &[".claude"]).is_err());
        assert_eq!(
            fs::read_to_string(path.join("notes.md")).unwrap(),
            "keep me"
        );
        fs::remove_file(path.join("notes.md")).unwrap();
        fs::write(path.join("SKILL.md"), "user edited").unwrap();
        assert!(install(home.path(), &[".claude"]).is_err());
        assert_eq!(
            fs::read_to_string(path.join("SKILL.md")).unwrap(),
            "user edited"
        );
    }

    #[test]
    fn refuses_to_overwrite_unmanaged_directory_or_symlink() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join(".claude/skills/aside");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("SKILL.md"), "user content").unwrap();
        assert!(install(home.path(), &[".claude"]).is_err());
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "user content"
        );
        #[cfg(unix)]
        {
            fs::remove_dir_all(&target).unwrap();
            std::os::unix::fs::symlink(home.path(), &target).unwrap();
            assert!(install(home.path(), &[".claude"]).is_err());
            assert!(target.is_symlink());
        }
    }
}
