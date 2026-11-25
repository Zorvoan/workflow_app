//! Small helpers: file-name sanitizing, slugs, path confinement.

use std::path::{Component, Path, PathBuf};

use crate::error::{AppError, AppResult};

/// Transliterate the Central-European characters common in EduPage schools.
pub fn fold_diacritics(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'ä' | 'à' | 'â' | 'ą' | 'å' => 'a',
            'Á' | 'Ä' | 'À' | 'Â' | 'Ą' | 'Å' => 'A',
            'č' | 'ć' | 'ç' => 'c',
            'Č' | 'Ć' | 'Ç' => 'C',
            'ď' => 'd',
            'Ď' => 'D',
            'é' | 'ě' | 'ë' | 'è' | 'ê' | 'ę' => 'e',
            'É' | 'Ě' | 'Ë' | 'È' | 'Ê' | 'Ę' => 'E',
            'í' | 'ï' | 'ì' | 'î' => 'i',
            'Í' | 'Ï' | 'Ì' | 'Î' => 'I',
            'ľ' | 'ĺ' | 'ł' => 'l',
            'Ľ' | 'Ĺ' | 'Ł' => 'L',
            'ň' | 'ń' | 'ñ' => 'n',
            'Ň' | 'Ń' | 'Ñ' => 'N',
            'ó' | 'ô' | 'ö' | 'ò' | 'ő' | 'õ' => 'o',
            'Ó' | 'Ô' | 'Ö' | 'Ò' | 'Ő' | 'Õ' => 'O',
            'ŕ' | 'ř' => 'r',
            'Ŕ' | 'Ř' => 'R',
            'š' | 'ś' | 'ş' => 's',
            'Š' | 'Ś' | 'Ş' => 'S',
            'ť' | 'ţ' => 't',
            'Ť' | 'Ţ' => 'T',
            'ú' | 'ů' | 'ü' | 'ù' | 'û' | 'ű' => 'u',
            'Ú' | 'Ů' | 'Ü' | 'Ù' | 'Û' | 'Ű' => 'U',
            'ý' | 'ÿ' => 'y',
            'Ý' | 'Ÿ' => 'Y',
            'ž' | 'ź' | 'ż' => 'z',
            'Ž' | 'Ź' | 'Ż' => 'Z',
            other => other,
        })
        .collect()
}

/// `"Programovanie – Kalkulačka v C++"` → `"programovanie-kalkulacka-v-c"`.
pub fn slugify(s: &str, max_len: usize) -> String {
    let folded = fold_diacritics(s).to_ascii_lowercase();
    let mut out = String::new();
    let mut dash = false;
    for c in folded.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let mut out = out.trim_matches('-').to_string();
    if out.len() > max_len {
        out.truncate(max_len);
        out = out.trim_end_matches('-').to_string();
    }
    out
}

/// A file name that is safe on Windows: no separators, reserved characters, or device names.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "LPT1", "LPT2", "LPT3"];
    let mut out = if cleaned.is_empty() || reserved.contains(&stem.as_str()) { format!("file_{cleaned}") } else { cleaned };
    if out.len() > 120 {
        let ext = Path::new(&out).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        out = format!("{}{}", &out[..out.floor_char_boundary(100)], ext);
    }
    out
}

/// Resolve a relative path *inside* `root`, rejecting absolute paths, `..`,
/// drive prefixes, and symlinks that escape the root.
pub fn resolve_within(root: &Path, rel: &str) -> AppResult<PathBuf> {
    let rel = rel.trim().replace('\\', "/");
    let rel_path = Path::new(&rel);
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }
    let mut out = root.to_path_buf();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            _ => return Err(AppError::Policy(format!("path '{rel}' is not allowed (must be relative, without '..')"))),
        }
    }
    // If any existing ancestor is a symlink pointing elsewhere, refuse.
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut probe = out.clone();
    while !probe.exists() {
        if !probe.pop() {
            break;
        }
    }
    if let Ok(canon) = probe.canonicalize() {
        if !canon.starts_with(&canon_root) {
            return Err(AppError::Policy(format!("path '{rel}' escapes the workspace")));
        }
    }
    Ok(out)
}

pub fn truncate_middle(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let half = max / 2;
    let head = &s[..s.floor_char_boundary(half)];
    let tail = &s[s.ceil_char_boundary(s.len() - half)..];
    format!("{head}\n… [{} bytes omitted] …\n{tail}", s.len() - head.len() - tail.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_folds_slovak_and_czech() {
        assert_eq!(slugify("Programovanie – Kalkulačka v C++", 100), "programovanie-kalkulacka-v-c");
        assert_eq!(slugify("Úloha č. 3: Ťažké řešení", 100), "uloha-c-3-tazke-reseni");
        assert_eq!(slugify("abc def ghi", 6), "abc-de");
    }

    #[test]
    fn safe_names() {
        assert_eq!(safe_file_name("../a/b:c.txt"), "_a_b_c.txt");
        assert_eq!(safe_file_name("CON.txt"), "file_CON.txt");
        assert_eq!(safe_file_name(""), "file_");
    }

    #[test]
    fn path_confinement() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_within(dir.path(), "src/main.rs").is_ok());
        assert!(resolve_within(dir.path(), "../evil").is_err());
        assert!(resolve_within(dir.path(), "/etc/passwd").is_err());
        assert!(resolve_within(dir.path(), "a/../../b").is_err());
        #[cfg(windows)]
        assert!(resolve_within(dir.path(), "C:\\Windows").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(resolve_within(dir.path(), "link/x.txt").is_err());
    }

    #[test]
    fn truncation_keeps_both_ends() {
        let s = "a".repeat(50) + &"b".repeat(50);
        let t = truncate_middle(&s, 20);
        assert!(t.starts_with("aaaaaaaaaa") && t.ends_with("bbbbbbbbbb"));
    }
}
