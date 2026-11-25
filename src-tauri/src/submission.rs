//! Submission preparation and delivery.
//!
//! Supported methods and what each one can honestly verify:
//!
//! * `edupage_message` – uploads the files to the student's EduPage cloud and
//!   sends the teacher a timeline message with the repository link and files.
//!   EduPage answers with the new message's `timelineid`; only then is the
//!   submission recorded as **confirmed**. This uses endpoints documented by
//!   the community `edupage-api` library. It is *not* EduPage's homework
//!   hand-in button.
//! * `manual_handin` – EduTask prepares a ZIP + link and opens EduPage; the
//!   student hands it in through EduPage's own homework UI and then tells
//!   EduTask, which records it as **self-reported** (never "confirmed").
//!
//! The homework hand-in form itself (the e-learning "odovzdať" flow) is not
//! documented anywhere public and is not automated; see docs/EDUPAGE.md.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::db::models::Assignment;
use crate::error::{AppError, AppResult};
use crate::github::files::{self, UploadFile};

pub const METHOD_MESSAGE: &str = "edupage_message";
pub const METHOD_MANUAL: &str = "manual_handin";
const TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Serialize)]
pub struct MethodInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    /// Whether EduPage itself returns a confirmation for this method.
    pub platform_confirmed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmissionFile {
    pub name: String,
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmissionPreview {
    pub assignment_id: String,
    pub title: String,
    pub teacher: Option<String>,
    pub repo_url: Option<String>,
    pub files: Vec<SubmissionFile>,
    pub methods: Vec<MethodInfo>,
    pub message: String,
    pub expectation: Option<String>,
    pub confirm_token: String,
    pub warnings: Vec<String>,
}

pub struct Pending {
    pub assignment_id: String,
    pub files: Vec<SubmissionFile>,
    pub created: Instant,
}

#[derive(Default)]
pub struct Confirmations(std::sync::Mutex<HashMap<String, Pending>>);

impl Confirmations {
    pub fn issue(&self, assignment_id: &str, files: Vec<SubmissionFile>) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        m.retain(|_, p| p.created.elapsed() < TOKEN_TTL);
        m.insert(token.clone(), Pending { assignment_id: assignment_id.into(), files, created: Instant::now() });
        token
    }

    /// Single use: a token is consumed whether or not submission then succeeds.
    pub fn redeem(&self, token: &str, assignment_id: &str) -> AppResult<Pending> {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let p = m.remove(token).ok_or_else(|| AppError::Policy("Submission was not confirmed in the review screen (or the confirmation expired). Open the review again.".into()))?;
        if p.assignment_id != assignment_id || p.created.elapsed() >= TOKEN_TTL {
            return Err(AppError::Policy("Confirmation does not match this assignment or has expired".into()));
        }
        Ok(p)
    }
}

const DOC_EXT: &[&str] = &["pdf", "docx", "doc", "pptx", "ppt", "xlsx", "odt", "odp", "ods"];

/// Build `submission/<slug>.zip` from the project (respecting .gitignore and
/// secret exclusions) and collect standalone documents.
pub fn package(project_dir: &Path, out_dir: &Path, slug: &str, known_secrets: &[String]) -> AppResult<(Vec<SubmissionFile>, Vec<String>)> {
    let list: Vec<UploadFile> = files::collect(project_dir)?;
    if list.is_empty() {
        return Err(AppError::InvalidInput("The project folder is empty. Build the assignment first.".into()));
    }
    let findings = files::scan_files(&list, known_secrets);
    if !findings.is_empty() {
        let f = &findings[0];
        return Err(AppError::Policy(format!("Possible secret ({}) in {} line {}. Remove it before submitting.", f.kind, f.path, f.line)));
    }
    std::fs::create_dir_all(out_dir)?;
    let zip_path = out_dir.join(format!("{slug}.zip"));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path)?);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for f in &list {
        zip.start_file(format!("{slug}/{}", f.rel_path), opts).map_err(|e| AppError::Internal(e.to_string()))?;
        zip.write_all(&std::fs::read(&f.abs_path)?)?;
    }
    zip.finish().map_err(|e| AppError::Internal(e.to_string()))?;

    let mut out = vec![SubmissionFile {
        name: format!("{slug}.zip"),
        size: std::fs::metadata(&zip_path)?.len(),
        path: zip_path.to_string_lossy().into_owned(),
    }];
    let mut warnings = Vec::new();
    for f in &list {
        let ext = Path::new(&f.rel_path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if DOC_EXT.contains(&ext.as_str()) && !f.rel_path.contains('/') {
            let dest: PathBuf = out_dir.join(crate::util::safe_file_name(&f.rel_path));
            std::fs::copy(&f.abs_path, &dest)?;
            out.push(SubmissionFile { name: f.rel_path.clone(), size: f.size, path: dest.to_string_lossy().into_owned() });
        }
    }
    if out[0].size > 50 * 1024 * 1024 {
        warnings.push("The ZIP is larger than EduPage's 50 MB upload limit; submit the repository link instead.".into());
    }
    Ok((out, warnings))
}

pub fn default_message(a: &Assignment, repo_url: Option<&str>) -> String {
    let mut s = format!("Hello,\n\nI am submitting my solution to the assignment \"{}\".", a.title);
    if let Some(url) = repo_url {
        s.push_str(&format!("\n\nRepository: {url}"));
    }
    s.push_str("\n\nThe project files are attached.\n\nThank you.");
    s
}

pub fn methods(a: &Assignment, connected: bool) -> Vec<MethodInfo> {
    let msg_reason = if !connected {
        Some("Not connected to EduPage".to_string())
    } else if a.teacher_ref.is_none() {
        Some("The teacher's EduPage account could not be identified from the assignment".to_string())
    } else {
        None
    };
    vec![
        MethodInfo {
            id: METHOD_MESSAGE.into(),
            label: "Send to teacher via EduPage message".into(),
            description: "Uploads the files to your EduPage cloud and messages the teacher with the files and repository link. EduPage confirms delivery. This does not press the homework's own hand-in button.".into(),
            available: msg_reason.is_none(),
            unavailable_reason: msg_reason,
            platform_confirmed: true,
        },
        MethodInfo {
            id: METHOD_MANUAL.into(),
            label: "Hand in on EduPage myself".into(),
            description: "EduTask prepares the ZIP and copies the repository link; you upload them in EduPage's homework view and then mark the assignment as submitted here. Recorded as self-reported.".into(),
            available: true,
            unavailable_reason: None,
            platform_confirmed: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_single_use_and_bound_to_assignment() {
        let c = Confirmations::default();
        let t = c.issue("a1", vec![]);
        assert!(c.redeem(&t, "a2").is_err());
        let t = c.issue("a1", vec![]);
        assert!(c.redeem(&t, "a1").is_ok());
        assert!(c.redeem(&t, "a1").is_err(), "second use rejected");
        assert!(c.redeem("forged", "a1").is_err());
    }

    #[test]
    fn package_refuses_secrets_and_zips_project() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("project");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("main.py"), "print(1)").unwrap();
        std::fs::write(proj.join("report.docx"), "PK").unwrap();
        let (files, _) = package(&proj, &d.path().join("out"), "hw", &[]).unwrap();
        assert_eq!(files[0].name, "hw.zip");
        assert!(files.iter().any(|f| f.name == "report.docx"));
        std::fs::write(proj.join("cfg.py"), "KEY='sk-ant-aaaaaaaaaaaaaaaaaaaaaaaaaaaa'").unwrap();
        assert!(matches!(package(&proj, &d.path().join("out"), "hw", &[]), Err(AppError::Policy(_))));
    }
}
