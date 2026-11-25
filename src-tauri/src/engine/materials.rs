//! Turns an assignment's materials folder into AI input.
//!
//! * PDFs and images are passed natively when the provider supports it.
//! * Office/OpenDocument files are converted to plain text (XML text runs).
//! * Source/text files are inlined.
//! * Archives are listed, never extracted or executed.

use std::io::Read;
use std::path::Path;

use base64::Engine;
use regex::Regex;

use crate::ai::Block;

const MAX_TEXT_PER_FILE: usize = 100 * 1024;
const MAX_TEXT_TOTAL: usize = 400 * 1024;
const MAX_IMAGE: u64 = 5 * 1024 * 1024;
const MAX_PDF: u64 = 25 * 1024 * 1024;

const TEXT_EXT: &[&str] = &[
    "txt", "md", "csv", "json", "xml", "yaml", "yml", "html", "htm", "css", "js", "ts", "jsx", "tsx", "py", "java", "c", "h", "cpp", "hpp",
    "cc", "cs", "rs", "php", "sql", "sh", "bat", "ps1", "kt", "go", "rb", "ini", "toml",
];

pub struct Materials {
    pub blocks: Vec<Block>,
    pub inventory: Vec<String>,
}

fn strip_xml(xml: &str, para_tag: &str) -> String {
    static TAG: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let tag = TAG.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let with_breaks = xml.replace(&format!("</{para_tag}>"), "\n");
    let text = tag.replace_all(&with_breaks, "");
    text.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'")
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract text from DOCX/PPTX/XLSX/ODT/ODP/ODS.
pub fn office_text(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let mut names: Vec<String> = (0..zip.len()).filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string())).collect();
    let (wanted, para): (Vec<String>, &str) = match ext.as_str() {
        "docx" => (vec!["word/document.xml".into()], "w:p"),
        "pptx" => {
            names.retain(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"));
            names.sort_by_key(|n| n.trim_start_matches("ppt/slides/slide").trim_end_matches(".xml").parse::<u32>().unwrap_or(0));
            (names, "a:p")
        }
        "xlsx" => (vec!["xl/sharedStrings.xml".into()], "si"),
        "odt" | "odp" | "ods" => (vec!["content.xml".into()], "text:p"),
        _ => return None,
    };
    let mut out = String::new();
    for (i, name) in wanted.iter().enumerate() {
        let mut xml = String::new();
        if let Ok(mut f) = zip.by_name(name) {
            // Guard against zip bombs: cap the decompressed read.
            let _ = (&mut f).take(20 * 1024 * 1024).read_to_string(&mut xml);
        }
        if ext == "pptx" {
            out.push_str(&format!("\n--- Slide {} ---\n", i + 1));
        }
        out.push_str(&strip_xml(&xml, para));
    }
    Some(out)
}

fn archive_listing(path: &Path) -> Option<String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let names: Vec<String> = (0..zip.len().min(200)).filter_map(|i| zip.by_index(i).ok().map(|f| format!("  {} ({} bytes)", f.name(), f.size()))).collect();
    Some(names.join("\n"))
}

pub fn collect(materials_dir: &Path, native_documents: bool) -> Materials {
    let mut blocks = Vec::new();
    let mut inventory = Vec::new();
    let mut text_budget = MAX_TEXT_TOTAL;
    let b64 = base64::engine::general_purpose::STANDARD;

    let mut entries: Vec<_> = std::fs::read_dir(materials_dir).map(|rd| rd.filter_map(Result::ok).collect()).unwrap_or_default();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "assignment.md" {
            continue; // already part of the prompt
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        inventory.push(format!("materials/{name} ({size} bytes)"));

        let mut push_text = |label: String, text: String| {
            if text_budget == 0 {
                return;
            }
            let t = crate::util::truncate_middle(&text, MAX_TEXT_PER_FILE.min(text_budget));
            text_budget = text_budget.saturating_sub(t.len());
            blocks.push(Block::Text(format!("<material file=\"{label}\">\n{t}\n</material>")));
        };

        match ext.as_str() {
            "pdf" if native_documents && size <= MAX_PDF => {
                if let Ok(bytes) = std::fs::read(&path) {
                    blocks.push(Block::Pdf { data_b64: b64.encode(bytes), title: name.clone() });
                }
            }
            "png" | "jpg" | "jpeg" | "gif" | "webp" if native_documents && size <= MAX_IMAGE => {
                if let Ok(bytes) = std::fs::read(&path) {
                    let media_type = if ext == "jpg" { "image/jpeg".to_string() } else { format!("image/{ext}") };
                    blocks.push(Block::Text(format!("Image attachment: materials/{name}")));
                    blocks.push(Block::Image { media_type, data_b64: b64.encode(bytes) });
                }
            }
            "docx" | "pptx" | "xlsx" | "odt" | "odp" | "ods" => {
                if let Some(t) = office_text(&path) {
                    push_text(format!("materials/{name}"), t);
                }
            }
            "zip" => {
                if let Some(l) = archive_listing(&path) {
                    push_text(format!("materials/{name}"), format!("ZIP archive (not extracted) containing:\n{l}"));
                }
            }
            e if TEXT_EXT.contains(&e) => {
                if let Ok(bytes) = std::fs::read(&path) {
                    push_text(format!("materials/{name}"), String::from_utf8_lossy(&bytes).into_owned());
                }
            }
            _ => {}
        }
    }
    Materials { blocks, inventory }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extracts_docx_text_and_inlines_source() {
        let dir = tempfile::tempdir().unwrap();
        let docx = dir.path().join("zadanie.docx");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&docx).unwrap());
        z.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(b"<w:document><w:body><w:p><w:r><w:t>Napiste program</w:t></w:r></w:p><w:p><w:r><w:t>Vstup: n &amp; m</w:t></w:r></w:p></w:body></w:document>").unwrap();
        z.finish().unwrap();
        std::fs::write(dir.path().join("sample.cpp"), "int main(){}").unwrap();
        std::fs::write(dir.path().join("assignment.md"), "skip me").unwrap();

        let m = collect(dir.path(), true);
        let all: String = m.blocks.iter().filter_map(|b| if let Block::Text(t) = b { Some(t.clone()) } else { None }).collect();
        assert!(all.contains("Napiste program\nVstup: n & m"));
        assert!(all.contains("int main(){}"));
        assert!(!all.contains("skip me"));
        assert_eq!(m.inventory.len(), 2);
    }

    #[test]
    fn pdf_only_native_when_supported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.pdf"), b"%PDF-1.4").unwrap();
        assert!(collect(dir.path(), true).blocks.iter().any(|b| matches!(b, Block::Pdf { .. })));
        assert!(!collect(dir.path(), false).blocks.iter().any(|b| matches!(b, Block::Pdf { .. })));
    }
}
