//! `.gitignore` generation: a base that always excludes secrets, plus
//! per-language build output, plus the user's own lines from Settings.

const BASE: &str = "# --- EduTask: secrets (never commit) ---
.env
.env.*
*.pem
*.key
*.pfx
*.p12
id_rsa*
id_ed25519*
secrets.*
credentials.*
# --- OS / editors ---
.DS_Store
Thumbs.db
desktop.ini
.vscode/
.idea/
*.swp
# --- EduTask internals ---
.edutask/
";

fn language_block(lang: &str) -> &'static str {
    match lang {
        "rust" => "target/\n",
        "cpp" | "c" => "build/\ncmake-build-*/\n*.o\n*.obj\n*.exe\n*.out\n*.pdb\n*.ilk\n",
        "csharp" => "bin/\nobj/\n*.user\n.vs/\n",
        "java" => "*.class\ntarget/\nbuild/\n.gradle/\nout/\n",
        "python" => "__pycache__/\n*.pyc\n.venv/\nvenv/\n.pytest_cache/\n*.egg-info/\n",
        "php" => "vendor/\n",
        "javascript" | "typescript" => "node_modules/\ndist/\n.cache/\nnpm-debug.log*\n",
        _ => "",
    }
}

pub fn generate(language: &str, extra: &str) -> String {
    let mut s = BASE.to_string();
    let block = language_block(language);
    if !block.is_empty() {
        s.push_str(&format!("# --- {language} ---\n{block}"));
    }
    if !extra.trim().is_empty() {
        s.push_str("# --- user settings ---\n");
        s.push_str(extra.trim());
        s.push('\n');
    }
    s
}

/// Keep the project's own lines and add any EduTask lines it lacks.
pub fn merge(existing: &str, generated: &str) -> String {
    let have: std::collections::HashSet<&str> = existing.lines().map(str::trim).collect();
    let mut out = existing.trim_end().to_string();
    let missing: Vec<&str> = generated.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty() && !have.contains(l.trim())).collect();
    if !missing.is_empty() {
        out.push_str("\n\n# --- added by EduTask ---\n");
        out.push_str(&missing.join("\n"));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_excludes_env_files() {
        for lang in ["rust", "python", "html", "unknown"] {
            let g = generate(lang, "");
            assert!(g.lines().any(|l| l == ".env"));
        }
        assert!(generate("rust", "").contains("target/"));
        assert!(generate("python", "*.sqlite").ends_with("*.sqlite\n"));
    }

    #[test]
    fn merge_is_idempotent() {
        let g = generate("python", "");
        let once = merge("node_modules/\n", &g);
        assert_eq!(merge(&once, &g), once);
        assert!(once.contains("node_modules/") && once.contains(".env"));
    }
}
