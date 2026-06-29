use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Clone, Deserialize)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    #[serde(deserialize_with = "deser_login")]
    pub author: String,
    pub url: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

fn deser_login<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    struct Author {
        login: String,
    }
    Author::deserialize(d).map(|a| a.login)
}

pub fn slug_from_url(url: &str) -> Option<String> {
    let s = url.trim_end_matches(".git");
    let s = if let Some(r) = s.strip_prefix("git@github.com:") {
        r
    } else if let Some(r) = s.strip_prefix("https://github.com/") {
        r
    } else {
        return None;
    };
    if s.contains('/') {
        Some(s.to_string())
    } else {
        None
    }
}

pub fn list_prs(repo_url: &str) -> Result<Vec<Pr>> {
    let slug = slug_from_url(repo_url)
        .with_context(|| format!("cannot extract owner/repo from URL: {repo_url}"))?;
    let out = Command::new("gh")
        .args([
            "pr", "list",
            "--repo", &slug,
            "--state", "open",
            "--json", "number,title,author,url,isDraft,createdAt",
        ])
        .output()
        .context("run gh (is gh installed and authenticated?)")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("{}", stderr.trim());
    }
    let prs: Vec<Pr> = serde_json::from_slice(&out.stdout).context("parse gh output")?;
    Ok(prs)
}

pub fn current_user() -> Result<String> {
    let out = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .context("run gh api user")?;
    if !out.status.success() {
        anyhow::bail!("gh not authenticated");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// Return just the date portion of an ISO 8601 timestamp, e.g. "2024-01-15".
pub fn pr_age(iso: &str) -> &str {
    iso.get(..10).unwrap_or(iso)
}
