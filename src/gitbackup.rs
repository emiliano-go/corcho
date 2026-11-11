use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STARTED: AtomicBool = AtomicBool::new(false);

/// Start the optional git backup loop. Configure with:
///   CORCHO_GIT_BACKUP_REPO=/path/to/repo
///   CORCHO_GIT_BACKUP_MINUTES=10
///   CORCHO_GIT_BACKUP_REMOTE=origin
///   CORCHO_GIT_BACKUP_BRANCH=main
pub fn start(data_dir: PathBuf) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Ok(repo) = std::env::var("CORCHO_GIT_BACKUP_REPO") else {
        return;
    };
    if repo.trim().is_empty() {
        return;
    }
    let minutes = std::env::var("CORCHO_GIT_BACKUP_MINUTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(10)
        .max(1);
    let remote = std::env::var("CORCHO_GIT_BACKUP_REMOTE").unwrap_or_else(|_| "origin".into());
    let branch = std::env::var("CORCHO_GIT_BACKUP_BRANCH").unwrap_or_else(|_| "main".into());
    let repo = PathBuf::from(repo);
    let _ = std::thread::Builder::new()
        .name("corcho-git-backup".into())
        .spawn(move || loop {
            if let Err(err) = backup_once(&data_dir, &repo, &remote, &branch) {
                eprintln!("git backup: {err}");
            }
            std::thread::sleep(Duration::from_secs(minutes * 60));
        });
}

fn backup_once(data_dir: &Path, repo: &Path, remote: &str, branch: &str) -> Result<()> {
    let workspace = crate::storage::load_workspace(data_dir)?;
    let value = backup_value(&workspace)?;
    let dir = repo.join("corcho");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("workspace.json");
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&value)?)?;
    std::fs::rename(&tmp, &path)?;
    let history = data_dir.join("iroh").join("history.jsonl");
    if history.exists() {
        let _ = std::fs::copy(history, dir.join("history.jsonl"));
    }

    let add = std::process::Command::new("git")
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["add", "corcho"])
        .status()?;
    if !add.success() {
        return Err(anyhow!("git add failed"));
    }
    let message = format!("corcho backup {}", now());
    let _ = std::process::Command::new("git")
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["commit", "-m", &message])
        .status()?;
    let push = std::process::Command::new("git")
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["push", remote, branch])
        .status()?;
    if !push.success() {
        return Err(anyhow!("git push failed"));
    }
    Ok(())
}

fn backup_value(workspace: &crate::model::Workspace) -> Result<Value> {
    let mut value = serde_json::to_value(workspace)?;
    value["settings"] = crate::irohsync::syncable_settings(&workspace.settings);
    Ok(value)
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_commits_and_pushes_to_a_remote() {
        use std::process::Command;
        let root = tempfile::tempdir().unwrap();
        let bare = root.path().join("remote.git");
        let repo = root.path().join("repo");
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            crate::storage::workspace_path(&data),
            serde_json::to_vec(&crate::model::Workspace::default()).unwrap(),
        )
        .unwrap();
        let run = |args: &[&str], dir: &std::path::Path| {
            let out = Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let out = Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(&bare)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = Command::new("git")
            .args(["init", "-b", "main"])
            .arg(&repo)
            .output()
            .unwrap();
        assert!(out.status.success());
        run(&["config", "user.email", "test@example.com"], &repo);
        run(&["config", "user.name", "Test"], &repo);
        let remote = bare.to_str().unwrap();
        run(&["remote", "add", "origin", remote], &repo);
        backup_once(&data, &repo, "origin", "main").unwrap();
        let out = Command::new("git")
            .current_dir(&bare)
            .args(["log", "--oneline"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("corcho backup"));
    }

    #[test]
    fn backup_strips_secrets() {
        let mut workspace = crate::model::Workspace::default();
        workspace
            .settings
            .ai_api_keys
            .insert("claude".into(), "secret".into());
        workspace.settings.repo_token_github = "token".into();
        let value = backup_value(&workspace).unwrap();
        let text = value.to_string();
        assert!(!text.contains("secret"));
        assert!(!text.contains("token"));
    }
}
