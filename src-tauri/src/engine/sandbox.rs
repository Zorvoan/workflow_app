//! Controlled execution of build/test commands.
//!
//! What this protects against, and what it does not:
//! * Commands are `{program, args}` pairs executed **without a shell**, so there
//!   is no shell interpolation, piping or chaining.
//! * Only an allowlist of compilers / interpreters / package managers may run,
//!   with per-tool subcommand rules (no `npm publish`, `cargo install`, global installs…).
//! * Package installs only run when "allow dependency installation" is enabled.
//! * The working directory is always the assignment's project folder; arguments
//!   that are absolute paths outside it or contain `..` are rejected.
//! * The environment is cleared and rebuilt from a small allowlist, so API keys
//!   or tokens in the user's environment are not visible to generated code.
//! * Every command has a timeout; on expiry the whole process tree is killed.
//! * Downloaded assignment files live in `materials/`, outside the project, and
//!   are never executed directly.
//!
//! This is process-level isolation, not a VM: generated code runs with the
//! user's privileges and network access. That limitation is documented in
//! SECURITY.md and shown in the UI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    pub fn display(&self) -> String {
        let mut s = self.program.clone();
        for a in &self.args {
            s.push(' ');
            if a.contains(' ') { s.push_str(&format!("\"{a}\"")) } else { s.push_str(a) }
        }
        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    Build,
    Dependency,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub allow_dependency_install: bool,
    pub timeout: Duration,
}

const OUTPUT_CAP: usize = 64 * 1024;

fn normalize_program(p: &str) -> String {
    let base = Path::new(p).file_name().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    for ext in [".exe", ".cmd", ".bat"] {
        if let Some(stripped) = base.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    base
}

fn first_non_flag(args: &[String]) -> Option<&str> {
    args.iter().map(String::as_str).find(|a| !a.starts_with('-'))
}

fn looks_absolute(a: &str) -> bool {
    let b = a.as_bytes();
    a.starts_with('/') || a.starts_with('\\') || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// Decide whether a command may run. Returns its class, or a human-readable reason.
pub fn classify(cmd: &CommandSpec, policy: &Policy) -> Result<CommandClass, String> {
    if cmd.program.contains('/') || cmd.program.contains('\\') {
        return Err(format!("'{}': programs must be given by name, not path", cmd.program));
    }
    for a in &cmd.args {
        // `=value` forms like `--out=/x` too.
        let value = a.split_once('=').map(|(_, v)| v).unwrap_or(a);
        if looks_absolute(value) && !value.starts_with("/std:") {
            return Err(format!("argument '{a}' is an absolute path; only paths inside the project are allowed"));
        }
        if value.replace('\\', "/").split('/').any(|seg| seg == "..") {
            return Err(format!("argument '{a}' leaves the project folder"));
        }
    }
    let prog = normalize_program(&cmd.program);
    let sub = first_non_flag(&cmd.args).unwrap_or("");
    let has_flag = |f: &str| cmd.args.iter().any(|a| a == f);
    let class = match prog.as_str() {
        "gcc" | "g++" | "clang" | "clang++" | "rustc" | "javac" | "java" | "jar" | "tsc" | "node" | "php" | "sqlite3" | "pandoc"
        | "cmake" | "make" | "mingw32-make" | "ninja" | "pytest" | "gradle" | "mvn" => CommandClass::Build,
        "python" | "python3" | "py" => {
            let module = cmd.args.windows(2).find(|w| w[0] == "-m").map(|w| w[1].as_str());
            let after_module = cmd.args.iter().skip_while(|a| a.as_str() != "-m").nth(2).map(String::as_str);
            if module == Some("pip") {
                if after_module == Some("install") { CommandClass::Dependency } else { return Err("only 'python -m pip install' is allowed for pip".into()) }
            } else if module == Some("ensurepip") || module == Some("venv") {
                CommandClass::Dependency
            } else {
                CommandClass::Build
            }
        }
        "pip" | "pip3" => {
            if sub == "install" { CommandClass::Dependency } else { return Err(format!("'pip {sub}' is not allowed")) }
        }
        "cargo" => match sub {
            "build" | "run" | "test" | "check" | "fmt" | "clippy" | "new" | "init" | "bench" | "doc" => CommandClass::Build,
            "fetch" | "add" | "update" | "generate-lockfile" => CommandClass::Dependency,
            other => return Err(format!("'cargo {other}' is not allowed")),
        },
        "npm" => {
            if has_flag("-g") || has_flag("--global") || has_flag("--location=global") {
                return Err("global npm installs are not allowed".into());
            }
            match sub {
                "run" | "test" | "start" | "run-script" | "init" => CommandClass::Build,
                "install" | "i" | "ci" | "add" => CommandClass::Dependency,
                other => return Err(format!("'npm {other}' is not allowed")),
            }
        }
        "npx" => {
            if has_flag("-g") { return Err("global npx is not allowed".into()) }
            CommandClass::Dependency // may download packages
        }
        "composer" => match sub {
            "install" | "require" | "update" | "dump-autoload" => CommandClass::Dependency,
            other => return Err(format!("'composer {other}' is not allowed")),
        },
        "dotnet" => match sub {
            "new" | "build" | "run" | "test" | "sln" | "format" | "--version" | "--info" => CommandClass::Build,
            "restore" | "add" => CommandClass::Dependency,
            other => return Err(format!("'dotnet {other}' is not allowed")),
        },
        other => return Err(format!("'{other}' is not on the list of allowed build tools")),
    };
    if class == CommandClass::Dependency && !policy.allow_dependency_install {
        return Err("dependency installation is disabled in Settings → Build".into());
    }
    Ok(class)
}

// ------------------------------------------------------------------ resolution

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
}

/// Find a program on PATH (with PATHEXT on Windows).
pub fn which(program: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into()).split(';').map(|e| e.to_ascii_lowercase()).collect()
    } else {
        vec![String::new()]
    };
    for dir in path_dirs() {
        for ext in &exts {
            let cand = dir.join(format!("{program}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// The Microsoft Store "python.exe" alias opens the Store instead of running Python.
fn is_store_stub(p: &Path) -> bool {
    p.to_string_lossy().to_ascii_lowercase().contains("windowsapps")
}

#[derive(Debug, Clone, Serialize)]
pub struct Toolchain {
    pub name: String,
    pub path: Option<String>,
    pub note: Option<String>,
}

pub fn detect_toolchains() -> Vec<Toolchain> {
    let names = [
        "cargo", "rustc", "g++", "gcc", "clang++", "cmake", "dotnet", "javac", "java", "mvn", "gradle", "python", "python3", "py", "php",
        "composer", "node", "npm", "sqlite3", "pandoc",
    ];
    names
        .iter()
        .map(|n| {
            let path = which(n);
            let stub = path.as_deref().is_some_and(is_store_stub) && n.starts_with("python");
            Toolchain {
                name: n.to_string(),
                path: if stub { None } else { path.map(|p| p.to_string_lossy().into_owned()) },
                note: stub.then(|| "Microsoft Store alias only – Python is not actually installed".into()),
            }
        })
        .collect()
}

// ------------------------------------------------------------------ execution

#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    pub command: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
}

impl CommandResult {
    /// The only definition of success: the process exited on its own with code 0.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && !self.cancelled
    }

    pub fn combined_output(&self, max: usize) -> String {
        let mut s = String::new();
        if !self.stdout.trim().is_empty() {
            s.push_str(&self.stdout);
        }
        if !self.stderr.trim().is_empty() {
            if !s.is_empty() { s.push('\n') }
            s.push_str(&self.stderr);
        }
        crate::util::truncate_middle(&s, max)
    }
}

const ENV_ALLOW: &[&str] = &[
    "PATH", "PATHEXT", "SYSTEMROOT", "SystemRoot", "WINDIR", "windir", "SYSTEMDRIVE", "COMSPEC", "NUMBER_OF_PROCESSORS", "PROCESSOR_ARCHITECTURE",
    "OS", "USERPROFILE", "HOME", "APPDATA", "LOCALAPPDATA", "PROGRAMDATA", "ProgramFiles", "ProgramFiles(x86)", "CommonProgramFiles",
    "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "JAVA_HOME", "DOTNET_ROOT", "GOPATH", "NVM_HOME", "NVM_SYMLINK", "PYTHONHOME",
    "LANG", "LC_ALL", "USER", "USERNAME", "LOGNAME", "SHELL", "TERM",
];

fn is_sensitive_name(k: &str) -> bool {
    let up = k.to_ascii_uppercase();
    ["TOKEN", "SECRET", "PASSWORD", "PASSWD", "API_KEY", "APIKEY", "CREDENTIAL", "PRIVATE"].iter().any(|s| up.contains(s))
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(mut r: R) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                // Keep the tail: errors are usually at the end.
                if buf.len() > OUTPUT_CAP * 2 {
                    buf.drain(..buf.len() - OUTPUT_CAP);
                }
            }
        }
    }
    let s = String::from_utf8_lossy(&buf).into_owned();
    if s.len() > OUTPUT_CAP { s[s.ceil_char_boundary(s.len() - OUTPUT_CAP)..].to_string() } else { s }
}

async fn kill_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).output();
        }
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("kill").args(["-9", &format!("-{pid}")]).output();
        }
    }
    let _ = child.kill().await;
}

/// Run an already-classified command in `cwd`.
pub async fn run(cmd: &CommandSpec, cwd: &Path, tmp_dir: &Path, policy: &Policy, cancel: &Arc<AtomicBool>) -> CommandResult {
    let started = Instant::now();
    let display = cmd.display();
    let fail = |msg: String| CommandResult {
        command: display.clone(),
        exit_code: None,
        timed_out: false,
        cancelled: false,
        duration_ms: started.elapsed().as_millis(),
        stdout: String::new(),
        stderr: msg,
    };

    let Some(resolved) = which(&cmd.program) else {
        return fail(format!("'{}' is not installed or not on PATH", cmd.program));
    };
    if is_store_stub(&resolved) && normalize_program(&cmd.program).starts_with("python") {
        return fail("Python is not installed (only the Microsoft Store alias was found). Install Python from python.org.".into());
    }
    let _ = std::fs::create_dir_all(tmp_dir);

    let mut env: BTreeMap<String, String> = std::env::vars().filter(|(k, _)| ENV_ALLOW.iter().any(|a| a.eq_ignore_ascii_case(k)) && !is_sensitive_name(k)).collect();
    let tmp = tmp_dir.to_string_lossy().into_owned();
    env.insert("TEMP".into(), tmp.clone());
    env.insert("TMP".into(), tmp.clone());
    env.insert("TMPDIR".into(), tmp);
    env.insert("CI".into(), "1".into());
    env.insert("NO_COLOR".into(), "1".into());
    env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    env.insert("PYTHONUNBUFFERED".into(), "1".into());
    env.insert("DOTNET_CLI_TELEMETRY_OPTOUT".into(), "1".into());
    env.insert("DOTNET_NOLOGO".into(), "1".into());

    let mut command = tokio::process::Command::new(&resolved);
    command.args(&cmd.args).current_dir(cwd).env_clear().envs(&env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        command.process_group(0);
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("failed to start '{}': {e}", cmd.program)),
    };
    let out = tokio::spawn(read_capped(child.stdout.take().expect("piped")));
    let err = tokio::spawn(read_capped(child.stderr.take().expect("piped")));

    let deadline = started + policy.timeout;
    let (mut timed_out, mut cancelled) = (false, false);
    let status = loop {
        tokio::select! {
            s = child.wait() => break s.ok(),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                if cancel.load(Ordering::Relaxed) { cancelled = true; }
                else if Instant::now() >= deadline { timed_out = true; }
                if cancelled || timed_out {
                    kill_tree(&mut child).await;
                    break child.wait().await.ok();
                }
            }
        }
    };
    let stdout = out.await.unwrap_or_default();
    let mut stderr = err.await.unwrap_or_default();
    if timed_out {
        stderr.push_str(&format!("\n[EduTask] command killed after {}s timeout", policy.timeout.as_secs()));
    }
    CommandResult {
        command: display,
        exit_code: if timed_out || cancelled { None } else { status.and_then(|s| s.code()) },
        timed_out,
        cancelled,
        duration_ms: started.elapsed().as_millis(),
        stdout,
        stderr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(p: &str, a: &[&str]) -> CommandSpec {
        CommandSpec { program: p.into(), args: a.iter().map(|s| s.to_string()).collect() }
    }
    fn pol(deps: bool) -> Policy {
        Policy { allow_dependency_install: deps, timeout: Duration::from_secs(5) }
    }

    #[test]
    fn allowlist() {
        assert_eq!(classify(&c("cargo", &["build", "--release"]), &pol(true)), Ok(CommandClass::Build));
        assert_eq!(classify(&c("g++", &["-std=c++17", "main.cpp", "-o", "app"]), &pol(true)), Ok(CommandClass::Build));
        assert_eq!(classify(&c("npm", &["install"]), &pol(true)), Ok(CommandClass::Dependency));
        assert!(classify(&c("npm", &["install"]), &pol(false)).is_err(), "deps gated by setting");
        assert!(classify(&c("npm", &["install", "-g", "x"]), &pol(true)).is_err());
        assert!(classify(&c("npm", &["publish"]), &pol(true)).is_err());
        assert!(classify(&c("cargo", &["install", "ripgrep"]), &pol(true)).is_err());
        assert!(classify(&c("powershell", &["-c", "x"]), &pol(true)).is_err());
        assert!(classify(&c("cmd", &["/c", "del"]), &pol(true)).is_err());
        assert!(classify(&c("curl", &["http://x"]), &pol(true)).is_err());
        assert!(classify(&c("/usr/bin/python3", &["a.py"]), &pol(true)).is_err());
        assert_eq!(classify(&c("python", &["-m", "pip", "install", "-r", "requirements.txt"]), &pol(true)), Ok(CommandClass::Dependency));
        assert!(classify(&c("python", &["-m", "pip", "uninstall", "x"]), &pol(true)).is_err());
        assert_eq!(classify(&c("python.exe", &["main.py"]), &pol(false)), Ok(CommandClass::Build));
    }

    #[test]
    fn rejects_paths_outside_project() {
        assert!(classify(&c("python", &["../../x.py"]), &pol(true)).is_err());
        assert!(classify(&c("gcc", &["/etc/passwd"]), &pol(true)).is_err());
        assert!(classify(&c("gcc", &["-o", "C:\\Windows\\x.exe"]), &pol(true)).is_err());
        assert!(classify(&c("gcc", &["--out=/tmp/x"]), &pol(true)).is_err());
    }

    #[test]
    fn env_filter() {
        assert!(is_sensitive_name("GITHUB_TOKEN"));
        assert!(is_sensitive_name("ANTHROPIC_API_KEY"));
        assert!(!is_sensitive_name("PATH"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_code_is_the_source_of_truth_and_timeout_kills() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let ok = run(&c("python3", &["-c", "print('hi')"]), dir.path(), &dir.path().join("tmp"), &pol(true), &cancel).await;
        assert!(ok.success(), "{ok:?}");
        assert!(ok.stdout.contains("hi"));
        let bad = run(&c("python3", &["-c", "import sys; print('boom', file=sys.stderr); sys.exit(3)"]), dir.path(), &dir.path().join("tmp"), &pol(true), &cancel).await;
        assert!(!bad.success());
        assert_eq!(bad.exit_code, Some(3));
        assert!(bad.stderr.contains("boom"));
        let slow = Policy { allow_dependency_install: true, timeout: Duration::from_secs(1) };
        let t = run(&c("python3", &["-c", "import time; time.sleep(30)"]), dir.path(), &dir.path().join("tmp"), &slow, &cancel).await;
        assert!(t.timed_out && !t.success());
        assert!(t.duration_ms < 10_000);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn secrets_in_parent_env_are_not_inherited() {
        std::env::set_var("EDUTASK_TEST_API_KEY", "sekret");
        let dir = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let r = run(&c("python3", &["-c", "import os; print(os.environ.get('EDUTASK_TEST_API_KEY'))"]), dir.path(), &dir.path().join("t"), &pol(true), &cancel).await;
        assert!(r.stdout.contains("None"), "{r:?}");
    }
}
