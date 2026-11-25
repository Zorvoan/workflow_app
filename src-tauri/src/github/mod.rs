//! GitHub REST API client.
//!
//! Authentication: a fine-grained or classic personal access token, or the
//! OAuth device flow (needs an OAuth App client id; no client secret is used).
//! Pushing uses the Git Data API (blobs → tree → commit → ref), so no local
//! git installation is required and the token never appears in a remote URL.

pub mod files;
pub mod gitignore;

use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use files::UploadFile;

const API: &str = "https://api.github.com";
const UA: &str = concat!("EduTask/", env!("CARGO_PKG_VERSION"));

pub struct GitHub {
    http: reqwest::Client,
    token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GithubUser {
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoRef {
    pub full_name: String,
    pub html_url: String,
    pub default_branch: String,
    pub private: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DevicePoll {
    Pending { interval: u64 },
    Authorized,
    Expired,
    Denied,
}

/// Render the naming template and make it a valid repository name.
pub fn repo_name(template: &str, subject: Option<&str>, title: &str, external_id: &str) -> String {
    let now = chrono::Local::now();
    let rendered = template
        .replace("{subject}", subject.unwrap_or(""))
        .replace("{title}", title)
        .replace("{year}", &now.format("%Y").to_string())
        .replace("{date}", &now.format("%Y-%m-%d").to_string())
        .replace("{id}", external_id);
    let slug = crate::util::slugify(&rendered, 90);
    if slug.is_empty() { format!("assignment-{}", crate::util::slugify(external_id, 20)) } else { slug }
}

async fn api_error(resp: reqwest::Response, what: &str) -> AppError {
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let msg = body.get("message").and_then(Value::as_str).unwrap_or("").to_string();
    let detail = body.pointer("/errors/0/message").and_then(Value::as_str).map(|d| format!(" ({d})")).unwrap_or_default();
    match status.as_u16() {
        401 => AppError::NotAuthenticated("GitHub rejected the token. Reconnect GitHub in Settings.".into()),
        403 if msg.contains("rate limit") => AppError::Network("GitHub API rate limit reached; try again later".into()),
        403 => AppError::Policy(format!("GitHub denied {what}: {msg}. The token may lack the 'repo' (classic) or 'Administration + Contents: write' (fine-grained) permission.")),
        _ => AppError::UnexpectedResponse(format!("GitHub {what} failed ({status}): {msg}{detail}")),
    }
}

impl GitHub {
    pub fn new(token: &str) -> AppResult<Self> {
        let http = reqwest::Client::builder().user_agent(UA).timeout(Duration::from_secs(120)).build()?;
        Ok(Self { http, token: token.to_string() })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{API}{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    pub async fn user(&self) -> AppResult<GithubUser> {
        let r = self.req(reqwest::Method::GET, "/user").send().await?;
        if !r.status().is_success() {
            return Err(api_error(r, "reading your account").await);
        }
        let v: Value = r.json().await?;
        Ok(GithubUser {
            login: v["login"].as_str().unwrap_or_default().to_string(),
            name: v["name"].as_str().map(String::from),
            avatar_url: v["avatar_url"].as_str().map(String::from),
        })
    }

    pub async fn get_repo(&self, full_name: &str) -> AppResult<Option<RepoRef>> {
        let r = self.req(reqwest::Method::GET, &format!("/repos/{full_name}")).send().await?;
        if r.status().as_u16() == 404 {
            return Ok(None);
        }
        if !r.status().is_success() {
            return Err(api_error(r, "reading the repository").await);
        }
        Ok(Some(parse_repo(&r.json().await?)))
    }

    /// Create a repository, appending `-2`, `-3`, … if the name is taken.
    pub async fn create_repo(&self, base_name: &str, private: bool, description: &str) -> AppResult<RepoRef> {
        for attempt in 1..=20 {
            let name = if attempt == 1 { base_name.to_string() } else { format!("{base_name}-{attempt}") };
            let r = self
                .req(reqwest::Method::POST, "/user/repos")
                .json(&json!({"name": name, "private": private, "description": description.chars().take(300).collect::<String>(), "auto_init": true, "has_wiki": false}))
                .send()
                .await?;
            if r.status().is_success() {
                return Ok(parse_repo(&r.json().await?));
            }
            if r.status().as_u16() == 422 {
                let body: Value = r.json().await.unwrap_or(Value::Null);
                if body.to_string().contains("already exists") {
                    continue;
                }
                return Err(AppError::UnexpectedResponse(format!("GitHub refused the repository name: {}", body["message"].as_str().unwrap_or(""))));
            }
            return Err(api_error(r, "creating the repository").await);
        }
        Err(AppError::Conflict("Could not find a free repository name".into()))
    }

    async fn head_sha(&self, full_name: &str, branch: &str) -> AppResult<String> {
        // Freshly auto-initialised repositories can take a moment to expose their ref.
        for attempt in 0..6 {
            let r = self.req(reqwest::Method::GET, &format!("/repos/{full_name}/git/ref/heads/{branch}")).send().await?;
            if r.status().is_success() {
                let v: Value = r.json().await?;
                return v.pointer("/object/sha").and_then(Value::as_str).map(String::from).ok_or_else(|| AppError::UnexpectedResponse("ref has no sha".into()));
            }
            if ![404, 409].contains(&r.status().as_u16()) || attempt == 5 {
                return Err(api_error(r, "reading the branch").await);
            }
            tokio::time::sleep(Duration::from_millis(800 * (attempt + 1))).await;
        }
        unreachable!()
    }

    /// Commit `files` as the complete content of `branch` (one new commit on top of the current head).
    pub async fn push_snapshot(&self, repo: &RepoRef, files: &[UploadFile], message: &str) -> AppResult<String> {
        let parent = self.head_sha(&repo.full_name, &repo.default_branch).await?;
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut tree = Vec::with_capacity(files.len());
        for f in files {
            let bytes = tokio::fs::read(&f.abs_path).await?;
            let r = self
                .req(reqwest::Method::POST, &format!("/repos/{}/git/blobs", repo.full_name))
                .json(&json!({"content": b64.encode(&bytes), "encoding": "base64"}))
                .send()
                .await?;
            if !r.status().is_success() {
                return Err(api_error(r, &format!("uploading {}", f.rel_path)).await);
            }
            let v: Value = r.json().await?;
            let sha = v["sha"].as_str().ok_or_else(|| AppError::UnexpectedResponse("blob has no sha".into()))?;
            tree.push(json!({"path": f.rel_path, "mode": "100644", "type": "blob", "sha": sha}));
        }
        let r = self.req(reqwest::Method::POST, &format!("/repos/{}/git/trees", repo.full_name)).json(&json!({"tree": tree})).send().await?;
        if !r.status().is_success() {
            return Err(api_error(r, "creating the file tree").await);
        }
        let tree_sha = r.json::<Value>().await?["sha"].as_str().unwrap_or_default().to_string();
        let r = self
            .req(reqwest::Method::POST, &format!("/repos/{}/git/commits", repo.full_name))
            .json(&json!({"message": message, "tree": tree_sha, "parents": [parent]}))
            .send()
            .await?;
        if !r.status().is_success() {
            return Err(api_error(r, "creating the commit").await);
        }
        let commit_sha = r.json::<Value>().await?["sha"].as_str().unwrap_or_default().to_string();
        let r = self
            .req(reqwest::Method::PATCH, &format!("/repos/{}/git/refs/heads/{}", repo.full_name, repo.default_branch))
            .json(&json!({"sha": commit_sha, "force": false}))
            .send()
            .await?;
        if !r.status().is_success() {
            return Err(api_error(r, "updating the branch").await);
        }
        // Verify: the branch must now point at our commit.
        let now = self.head_sha(&repo.full_name, &repo.default_branch).await?;
        if now != commit_sha {
            return Err(AppError::UnexpectedResponse("GitHub branch does not point at the new commit after push".into()));
        }
        Ok(commit_sha)
    }
}

fn parse_repo(v: &Value) -> RepoRef {
    RepoRef {
        full_name: v["full_name"].as_str().unwrap_or_default().to_string(),
        html_url: v["html_url"].as_str().unwrap_or_default().to_string(),
        default_branch: v["default_branch"].as_str().unwrap_or("main").to_string(),
        private: v["private"].as_bool().unwrap_or(true),
    }
}

// ------------------------------------------------------------ device flow

pub async fn device_start(client_id: &str) -> AppResult<DeviceCode> {
    let http = reqwest::Client::builder().user_agent(UA).build()?;
    let r = http
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .form(&[("client_id", client_id), ("scope", "repo")])
        .send()
        .await?;
    if !r.status().is_success() {
        return Err(AppError::UnexpectedResponse(format!("GitHub device flow failed ({}). Is the OAuth client id correct and device flow enabled?", r.status())));
    }
    let v: Value = r.json().await?;
    if let Some(err) = v.get("error").and_then(Value::as_str) {
        return Err(AppError::UnexpectedResponse(format!("GitHub device flow error: {err}")));
    }
    Ok(serde_json::from_value(v)?)
}

/// One poll. Returns the access token when authorized.
pub async fn device_poll(client_id: &str, device_code: &str) -> AppResult<(DevicePoll, Option<String>)> {
    let http = reqwest::Client::builder().user_agent(UA).build()?;
    let v: Value = http
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[("client_id", client_id), ("device_code", device_code), ("grant_type", "urn:ietf:params:oauth:grant-type:device_code")])
        .send()
        .await?
        .json()
        .await?;
    if let Some(tok) = v.get("access_token").and_then(Value::as_str) {
        return Ok((DevicePoll::Authorized, Some(tok.to_string())));
    }
    let interval = v.get("interval").and_then(Value::as_u64).unwrap_or(5);
    Ok(match v.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => (DevicePoll::Pending { interval }, None),
        Some("slow_down") => (DevicePoll::Pending { interval: interval + 5 }, None),
        Some("expired_token") => (DevicePoll::Expired, None),
        Some("access_denied") => (DevicePoll::Denied, None),
        Some(other) => return Err(AppError::UnexpectedResponse(format!("GitHub device flow error: {other}"))),
        None => return Err(AppError::UnexpectedResponse("Unexpected GitHub device flow response".into())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names() {
        assert_eq!(repo_name("{subject}-{title}", Some("Programovanie"), "Kalkulačka v C++", "777"), "programovanie-kalkulacka-v-c");
        assert_eq!(repo_name("{title}", None, "!!!", "777"), "assignment-777");
        assert!(repo_name("{year}-{title}", None, "x", "1").starts_with(&chrono::Local::now().format("%Y").to_string()));
    }
}
