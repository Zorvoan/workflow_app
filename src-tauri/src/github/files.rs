//! Selects which project files are uploaded and blocks anything that looks
//! like a secret.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Serialize;

use crate::error::{AppError, AppResult};

pub const MAX_FILE: u64 = 50 * 1024 * 1024;
const MAX_TOTAL: u64 = 500 * 1024 * 1024;

/// Directories never uploaded, even if the project's .gitignore forgets them.
const ALWAYS_SKIP_DIRS: &[&str] = &[".git", ".edutask", "node_modules", "target", "bin", "obj", "__pycache__", ".venv", "venv", ".pytest_cache", ".gradle", ".vs", "vendor"];
const ALWAYS_SKIP_FILE_PREFIX: &[&str] = &[".env", "id_rsa", "id_ed25519"];
const ALWAYS_SKIP_EXT: &[&str] = &["pem", "key", "pfx", "p12", "pyc", "o", "obj", "pdb", "ilk", "class"];

#[derive(Debug, Clone, Serialize)]
pub struct UploadFile {
    pub rel_path: String,
    #[serde(skip)]
    pub abs_path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretFinding {
    pub path: String,
    pub line: usize,
    pub kind: String,
}

fn patterns() -> &'static [(Regex, &'static str)] {
    static P: std::sync::OnceLock<Vec<(Regex, &'static str)>> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        [
            (r"gh[pousr]_[A-Za-z0-9]{30,}", "GitHub token"),
            (r"github_pat_[A-Za-z0-9_]{40,}", "GitHub token"),
            (r"sk-ant-[A-Za-z0-9_\-]{20,}", "Anthropic API key"),
            (r"\bsk-[A-Za-z0-9]{32,}", "API key"),
            (r"AKIA[0-9A-Z]{16}", "AWS access key"),
            (r"xox[baprs]-[A-Za-z0-9-]{10,}", "Slack token"),
            (r"AIza[0-9A-Za-z_\-]{35}", "Google API key"),
            (r"-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----", "private key"),
        ]
        .into_iter()
        .map(|(re, k)| (Regex::new(re).unwrap(), k))
        .collect()
    })
}

/// Scan text for credential patterns and for the user's own stored secrets.
pub fn scan_text(rel: &str, text: &str, known_secrets: &[String]) -> Vec<SecretFinding> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        for (re, kind) in patterns() {
            if re.is_match(line) {
                out.push(SecretFinding { path: rel.into(), line: i + 1, kind: (*kind).into() });
            }
        }
        if known_secrets.iter().any(|s| line.contains(s.as_str())) {
            out.push(SecretFinding { path: rel.into(), line: i + 1, kind: "one of your stored EduTask credentials".into() });
        }
    }
    out
}

/// Walk the project honoring its `.gitignore`, then apply hard exclusions.
pub fn collect(project_dir: &Path) -> AppResult<Vec<UploadFile>> {
    let mut files = Vec::new();
    let mut total = 0u64;
    let walker = ignore::WalkBuilder::new(project_dir)
        .hidden(false)
        .git_ignore(true)
        .require_git(false)
        .add_custom_ignore_filename(".gitignore")
        .follow_links(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(e.file_type().is_some_and(|t| t.is_dir()) && ALWAYS_SKIP_DIRS.contains(&name.as_ref()))
        })
        .build();
    for entry in walker {
        let entry = entry.map_err(|e| AppError::Internal(e.to_string()))?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if ALWAYS_SKIP_FILE_PREFIX.iter().any(|p| name.starts_with(p)) || ALWAYS_SKIP_EXT.contains(&ext.as_str()) {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE {
            return Err(AppError::Policy(format!("{} is larger than 50 MB; add it to .gitignore or shrink it", path.display())));
        }
        total += size;
        if total > MAX_TOTAL {
            return Err(AppError::Policy("Project is larger than 500 MB; check that build output is ignored".into()));
        }
        let rel = path.strip_prefix(project_dir).unwrap_or(path).to_string_lossy().replace('\\', "/");
        files.push(UploadFile { rel_path: rel, abs_path: path.to_path_buf(), size });
    }
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(files)
}

pub fn scan_files(files: &[UploadFile], known_secrets: &[String]) -> Vec<SecretFinding> {
    files
        .iter()
        .filter(|f| f.size <= 5 * 1024 * 1024)
        .filter_map(|f| std::fs::read(&f.abs_path).ok().map(|b| (f, b)))
        .filter(|(_, b)| !b.contains(&0)) // skip binaries
        .flat_map(|(f, b)| scan_text(&f.rel_path, &String::from_utf8_lossy(&b), known_secrets))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_secrets_and_build_output() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::create_dir_all(p.join("target/debug")).unwrap();
        std::fs::create_dir_all(p.join("node_modules/x")).unwrap();
        std::fs::write(p.join("src/main.rs"), "fn main(){}").unwrap();
        std::fs::write(p.join("target/debug/app"), "bin").unwrap();
        std::fs::write(p.join("node_modules/x/i.js"), "x").unwrap();
        std::fs::write(p.join(".env"), "KEY=1").unwrap();
        std::fs::write(p.join(".env.local"), "KEY=1").unwrap();
        std::fs::write(p.join("server.key"), "k").unwrap();
        std::fs::write(p.join("notes.log"), "l").unwrap();
        std::fs::write(p.join(".gitignore"), "*.log\n").unwrap();
        let files: Vec<String> = collect(p).unwrap().into_iter().map(|f| f.rel_path).collect();
        assert_eq!(files, vec![".gitignore", "src/main.rs"]);
    }

    #[test]
    fn detects_tokens_and_own_credentials() {
        let f = scan_text("a.py", "token = 'ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'\nok\npw = 'MySchoolPass1'", &["MySchoolPass1".into()]);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].line, 1);
        assert_eq!(f[1].line, 3);
        assert!(scan_text("b.py", "print('hello')", &[]).is_empty());
    }
}
