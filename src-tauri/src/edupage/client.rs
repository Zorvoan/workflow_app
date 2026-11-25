//! HTTP client for one EduPage school.
//!
//! EduPage has **no official public API for students**. Everything here mirrors
//! what EduPage's own web pages do, as documented by the community-maintained
//! `edupage-api` Python library (github.com/EdupageAPI/edupage-api, v0.13.3):
//!
//! | operation            | request                                                         |
//! |----------------------|-----------------------------------------------------------------|
//! | login (current)      | `GET /login/?cmd=MainLogin`, then RPC `akcia=getToken` + `akcia=login` |
//! | login (legacy)       | `POST /login/edubarLogin.php` with `csrfauth`, `username`, `password` |
//! | resume session       | `PHPSESSID` cookie + `GET /user/`                               |
//! | user data / homework | `userhome({...})` JSON embedded in `/user/`                     |
//! | older timeline items | `POST /timeline/?module=todo&akcia=getData` with `datefrom`     |
//! | upload a file        | `POST /timeline/?akcia=uploadAtt` (multipart field `att`)       |
//! | send a message       | RPC `POST /timeline/?akcia=createItem` (`typ=sprava`)           |
//!
//! Two-factor authentication and CAPTCHA are never bypassed: when EduPage asks
//! for them, the caller is told to use "Sign in with browser" instead, where the
//! user completes EduPage's own login page.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::CONTENT_TYPE;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use super::codec;
use super::parse;
use crate::error::{AppError, AppResult};

const USER_AGENT: &str = concat!("Mozilla/5.0 (compatible; EduTask/", env!("CARGO_PKG_VERSION"), ")");
pub const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
const FORM: &str = "application/x-www-form-urlencoded";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum LoginOutcome {
    LoggedIn { user_id: Option<String>, subdomain: String },
    /// EduPage requires a second factor; complete it with "Sign in with browser".
    TwoFactorRequired,
    /// EduPage demands a CAPTCHA; complete it with "Sign in with browser".
    CaptchaRequired,
}

#[derive(Debug, Clone, Serialize)]
pub struct CloudFile {
    pub file: String,
    pub name: String,
    pub url: String,
}

pub struct EdupageClient {
    http: reqwest::Client,
    jar: Arc<Jar>,
    pub subdomain: String,
    pub userhome: Option<Value>,
}

impl EdupageClient {
    pub fn new(subdomain: &str) -> AppResult<Self> {
        let jar = Arc::new(Jar::default());
        let http = reqwest::Client::builder()
            .cookie_provider(jar.clone())
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { http, jar, subdomain: subdomain.to_string(), userhome: None })
    }

    fn base(&self) -> String {
        format!("https://{}.edupage.org", self.subdomain)
    }

    fn base_url(&self) -> url::Url {
        url::Url::parse(&self.base()).expect("subdomain validated by settings")
    }

    /// The `PHPSESSID` cookie for the school, used to persist the session.
    pub fn session_id(&self) -> Option<String> {
        let header = self.jar.cookies(&self.base_url())?;
        header.to_str().ok()?.split(';').map(str::trim).find_map(|kv| kv.strip_prefix("PHPSESSID=").map(String::from))
    }

    pub fn user_id(&self) -> Option<String> {
        self.userhome.as_ref()?.get("userid")?.as_str().map(String::from)
    }

    // ------------------------------------------------------------------ login

    pub async fn login(&mut self, username: &str, password: &str) -> AppResult<LoginOutcome> {
        if self.subdomain.is_empty() {
            // login1.edupage.org redirects to the user's school.
            self.subdomain = "login1".into();
        }
        let response = match self.login_rpc(username, password).await? {
            Some(r) => r,
            None => self.login_legacy(username, password).await?,
        };

        let final_url = response.url().clone();
        if self.subdomain == "login1" {
            if let Some(host) = final_url.host_str() {
                self.subdomain = host.trim_end_matches(".edupage.org").to_string();
            }
        }
        let url_str = final_url.as_str().to_string();
        if url_str.contains("twofactor") {
            return Ok(LoginOutcome::TwoFactorRequired);
        }
        if url_str.contains("cap=1") || url_str.contains("lerr=b43b43") {
            return Ok(LoginOutcome::CaptchaRequired);
        }
        if url_str.contains("bad=1") {
            return Err(AppError::NotAuthenticated("EduPage rejected the username or password".into()));
        }
        let html = response.text().await?;
        let home = parse::extract_userhome(&html).map_err(|_| {
            AppError::NotAuthenticated("Login did not succeed. Check the school URL, username and password.".into())
        })?;
        self.userhome = Some(home);
        Ok(LoginOutcome::LoggedIn { user_id: self.user_id(), subdomain: self.subdomain.clone() })
    }

    /// Mirrors EduPage's `mainlogin.js`. Returns `None` when the RPC flow is not
    /// available so the legacy form login can be tried.
    async fn login_rpc(&self, username: &str, password: &str) -> AppResult<Option<reqwest::Response>> {
        let base = self.base();
        let r = self.http.get(format!("{base}/login/?cmd=MainLogin")).send().await?;
        if !r.status().is_success() {
            return Ok(None);
        }
        let token_params = json!({ "username": username, "edupage": "" }).to_string();
        let r = self
            .http
            .post(format!("{base}/login/?cmd=MainLogin&akcia=getToken"))
            .header(CONTENT_TYPE, FORM)
            .body(codec::encode_request_body(&[("rpcparams", &token_params)]))
            .send()
            .await?;
        if !r.status().is_success() {
            return Ok(None);
        }
        let Some(token) = parse_rpc(&r.text().await?).and_then(|v| v.get("token").and_then(Value::as_str).map(String::from)) else {
            return Ok(None);
        };
        let login_params = json!({
            "username": username, "password": password, "userToken": token,
            "edupage": "", "ctxt": "", "tu": null, "gu": null, "au": null
        })
        .to_string();
        let r = self
            .http
            .post(format!("{base}/login/?cmd=MainLogin&akcia=login"))
            .header(CONTENT_TYPE, FORM)
            .body(codec::encode_request_body(&[("rpcparams", &login_params)]))
            .send()
            .await?;
        if !r.status().is_success() {
            return Ok(None);
        }
        let Some(resp) = parse_rpc(&r.text().await?) else { return Ok(None) };
        let error_id = resp.pointer("/err/error_id").and_then(Value::as_str);
        let Some(redirect) = resp.get("redirectUrl").and_then(Value::as_str) else {
            if error_id.is_some() && error_id != Some("invalid_token") {
                return Err(AppError::NotAuthenticated("EduPage rejected the username or password".into()));
            }
            return Ok(None);
        };
        if error_id == Some("invalid_token") {
            return Ok(None);
        }
        let target = self.base_url().join(redirect).map_err(|e| AppError::UnexpectedResponse(format!("bad redirect: {e}")))?;
        if !parse::is_school_host(target.as_str()) {
            return Err(AppError::UnexpectedResponse("EduPage redirected the login to an unexpected host".into()));
        }
        Ok(Some(self.http.get(target).send().await?))
    }

    async fn login_legacy(&self, username: &str, password: &str) -> AppResult<reqwest::Response> {
        let base = self.base();
        let page = self.http.get(format!("{base}/login/?cmd=MainLogin")).send().await?.text().await?;
        let csrf = page
            .split("\"csrftoken\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .ok_or_else(|| AppError::UnexpectedResponse("EduPage did not provide a login token. Is the school URL correct?".into()))?;
        Ok(self
            .http
            .post(format!("{base}/login/edubarLogin.php"))
            .form(&[("csrfauth", csrf), ("username", username), ("password", password)])
            .send()
            .await?)
    }

    /// Restore a session from a stored (or browser-obtained) `PHPSESSID`.
    pub async fn resume(&mut self, session_id: &str) -> AppResult<()> {
        if session_id.is_empty() || !session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ',') {
            return Err(AppError::InvalidInput("Invalid EduPage session id".into()));
        }
        let url = self.base_url();
        self.jar.add_cookie_str(&format!("PHPSESSID={session_id}; Domain={}.edupage.org; Path=/; Secure", self.subdomain), &url);
        self.refresh().await
    }

    /// Reload `/user/` and the embedded user data. Fails with `NotAuthenticated` if the session expired.
    pub async fn refresh(&mut self) -> AppResult<()> {
        let html = self.http.get(format!("{}/user/", self.base())).send().await?.error_for_status()?.text().await?;
        self.userhome = Some(parse::extract_userhome(&html)?);
        Ok(())
    }

    pub async fn logout(&mut self) {
        let _ = self.http.post(format!("{}/login/logout.php", self.base())).send().await;
        self.userhome = None;
    }

    // ------------------------------------------------------------- assignments

    /// Timeline items older than what `/user/` embeds. Best effort: this is an
    /// undocumented endpoint, so failures are reported but not fatal.
    pub async fn timeline_history(&self, days: u32) -> AppResult<(Vec<Value>, Value)> {
        let from = (chrono::Local::now() - chrono::Duration::days(days as i64)).format("%Y-%m-%d").to_string();
        let r = self
            .http
            .post(format!("{}/timeline/?module=todo&filterTab=&akcia=getData&filterTab=messages", self.base()))
            .header(CONTENT_TYPE, FORM)
            .body(codec::encode_form(&[("datefrom", &from)]))
            .send()
            .await?
            .error_for_status()?;
        let v: Value = serde_json::from_str(&r.text().await?)?;
        let items = v.get("timelineItems").and_then(Value::as_array).cloned().ok_or_else(|| {
            AppError::UnexpectedResponse("timeline history response has no timelineItems".into())
        })?;
        Ok((items, v.get("timelineUserProps").cloned().unwrap_or(Value::Null)))
    }

    // ------------------------------------------------------------------ files

    /// Download a school-hosted file to `dest`. Only `https://*.edupage.org` URLs
    /// are fetched, so session cookies never leave EduPage.
    pub async fn download(&self, url: &str, dest: &Path) -> AppResult<u64> {
        if !parse::is_school_host(url) {
            return Err(AppError::Policy("Not an EduPage-hosted file; open it in the browser instead".into()));
        }
        let mut resp = self.http.get(url).timeout(Duration::from_secs(600)).send().await?.error_for_status()?;
        if !parse::is_school_host(resp.url().as_str()) {
            return Err(AppError::Policy("Download redirected outside EduPage; not followed".into()));
        }
        if resp.url().path().starts_with("/login") {
            return Err(AppError::NotAuthenticated("EduPage asked to log in again while downloading".into()));
        }
        if let Some(len) = resp.content_length() {
            if len > MAX_DOWNLOAD_BYTES {
                return Err(AppError::Policy(format!("File is larger than {} MB", MAX_DOWNLOAD_BYTES / 1024 / 1024)));
            }
        }
        let tmp = dest.with_extension("part");
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut total = 0u64;
        while let Some(chunk) = resp.chunk().await? {
            total += chunk.len() as u64;
            if total > MAX_DOWNLOAD_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(AppError::Policy("File exceeded the download size limit".into()));
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&tmp, dest).await?;
        Ok(total)
    }

    /// Upload a file to the user's EduPage cloud (max 50 MB, EduPage's limit).
    pub async fn upload(&self, path: &Path) -> AppResult<CloudFile> {
        let bytes = tokio::fs::read(path).await?;
        if bytes.len() > 50 * 1024 * 1024 {
            return Err(AppError::Policy(format!("{} is larger than EduPage's 50 MB limit", path.display())));
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let mime = mime_guess::from_path(path).first_or_octet_stream().to_string();
        let part = reqwest::multipart::Part::bytes(bytes).file_name(name.clone()).mime_str(&mime)?;
        let form = reqwest::multipart::Form::new().part("att", part);
        let text = self
            .http
            .post(format!("{}/timeline/?akcia=uploadAtt", self.base()))
            .multipart(form)
            .timeout(Duration::from_secs(600))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let v: Value = serde_json::from_str(&text).map_err(|_| AppError::UnexpectedResponse("EduPage upload returned non-JSON".into()))?;
        if v.get("status").and_then(Value::as_str) != Some("ok") {
            return Err(AppError::UnexpectedResponse(format!("EduPage refused the upload of {name}")));
        }
        let file = v.pointer("/data/file").and_then(Value::as_str).ok_or_else(|| AppError::UnexpectedResponse("upload response has no file path".into()))?;
        Ok(CloudFile {
            file: file.to_string(),
            name: v.pointer("/data/name").and_then(Value::as_str).unwrap_or(&name).to_string(),
            url: format!("{}{}", self.base(), file),
        })
    }

    /// Send a timeline message. Returns EduPage's `timelineid` for the new item,
    /// which is our proof that EduPage accepted it.
    pub async fn send_message(&self, recipient: &str, text: &str, attachments: &[CloudFile]) -> AppResult<String> {
        let atts: serde_json::Map<String, Value> = attachments.iter().map(|a| (a.file.clone(), Value::String(a.name.clone()))).collect();
        let atts = Value::Object(atts).to_string();
        let body = codec::encode_request_body(&[
            ("selectedUser", recipient),
            ("text", text),
            ("attachements", &atts),
            ("receipt", "0"),
            ("typ", "sprava"),
        ]);
        let raw = self
            .http
            .post(format!("{}/timeline/?=&akcia=createItem&eqav=1&maxEqav=7", self.base()))
            .header(CONTENT_TYPE, FORM)
            .body(body)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let decoded = codec::decode_response(&raw).unwrap_or_default();
        if decoded.trim() == "0" {
            return Err(AppError::UnexpectedResponse("EduPage returned an error response for the message".into()));
        }
        let v: Value = serde_json::from_str(&decoded)?;
        v.pointer("/changes/0/timelineid")
            .and_then(|t| t.as_str().map(String::from).or_else(|| t.as_i64().map(|n| n.to_string())))
            .ok_or_else(|| AppError::UnexpectedResponse("EduPage did not confirm the message (no timelineid in response)".into()))
    }
}

fn parse_rpc(text: &str) -> Option<Value> {
    codec::decode_response(text).and_then(|t| serde_json::from_str(&t).ok())
}
