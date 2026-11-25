//! Pure parsing of EduPage data. No I/O here, so it is fully unit-tested and is
//! the first place to update if EduPage changes its page structure.
//!
//! Data source: after login, `https://{school}.edupage.org/user/` embeds a
//! `userhome({...});` JavaScript call whose JSON argument contains:
//! * `items`      – timeline items; homework has `typ == "homework"` and a JSON
//!                  string in `data` with `nazov` (title), `popis` (description),
//!                  `date` (due date), `predmetid` (subject id), `superid`, …
//! * `dbi`        – lookup tables (`subjects`, `teachers`, `classes`)
//! * `userProps`  – per-item user state (`doneMaxCas` = marked as done)
//! * `userid`     – e.g. `Student12345`
//!
//! These field names come from the community `edupage-api` library and the
//! page itself; they are not an official contract. Every field is optional here.

use std::collections::{BTreeMap, HashMap};

use regex::Regex;
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::db::models::{RemoteAssignmentRow, RemoteAttachment};
use crate::error::{AppError, AppResult};

pub const PLATFORM: &str = "edupage";

/// Extract the JSON object passed to `userhome(` in the `/user/` page.
pub fn extract_userhome(html: &str) -> AppResult<Value> {
    let start = html
        .find("userhome(")
        .ok_or_else(|| AppError::NotAuthenticated("EduPage did not return the user home page (session invalid or expired)".into()))?;
    // EduPage emits raw tabs/newlines inside strings, which strict JSON rejects.
    let rest: String = html[start + "userhome(".len()..].chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let mut stream = serde_json::Deserializer::from_str(&rest).into_iter::<Value>();
    match stream.next() {
        Some(Ok(v)) if v.is_object() => Ok(v),
        _ => Err(AppError::UnexpectedResponse("Could not parse EduPage user data; EduPage may have changed its page format".into())),
    }
}

pub fn userhome_items(userhome: &Value) -> Vec<Value> {
    userhome.get("items").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn id_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `data` is usually a JSON string, sometimes already an object.
fn item_data(item: &Value) -> Value {
    match item.get("data") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v @ Value::Object(_)) => v.clone(),
        _ => Value::Null,
    }
}

fn subject_name(dbi: &Value, id: &str) -> Option<String> {
    let s = dbi.get("subjects")?.get(id)?;
    str_field(s, "name").or_else(|| str_field(s, "short")).map(String::from)
}

fn teacher_ref_by_name(dbi: &Value, name: &str) -> Option<String> {
    let teachers = dbi.get("teachers")?.as_object()?;
    let wanted = name.trim().to_lowercase();
    teachers.iter().find_map(|(id, t)| {
        let full = format!("{} {}", str_field(t, "firstname").unwrap_or(""), str_field(t, "lastname").unwrap_or("")).trim().to_lowercase();
        let reversed = format!("{} {}", str_field(t, "lastname").unwrap_or(""), str_field(t, "firstname").unwrap_or("")).trim().to_lowercase();
        (full == wanted || reversed == wanted).then(|| format!("Ucitel{id}"))
    })
}

pub fn html_to_text(html: &str) -> String {
    if !html.contains('<') {
        return html.trim().to_string();
    }
    html2text::from_read(html.as_bytes(), 100).trim().to_string()
}

fn absolutize(url: &str, base: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.starts_with("javascript:") || url.starts_with("mailto:") || url.starts_with('#') || url.starts_with("data:") {
        return None;
    }
    if url.starts_with("//") {
        return Some(format!("https:{url}"));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(url.to_string());
    }
    url::Url::parse(base).ok()?.join(url).ok().map(|u| u.to_string())
}

fn file_name_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|mut s| s.next_back().map(String::from)))
        .filter(|s| !s.is_empty())
        .map(|s| url::form_urlencoded::parse(s.as_bytes()).map(|(k, v)| if v.is_empty() { k.into_owned() } else { format!("{k}={v}") }).collect::<String>())
        .unwrap_or_else(|| "attachment".into())
}

/// Attachments appear under different keys/shapes depending on the item type.
fn collect_attachments(data: &Value, base: &str, out: &mut Vec<RemoteAttachment>) {
    for key in ["attachements", "attachments", "prilohy", "files"] {
        match data.get(key) {
            Some(Value::Object(map)) => {
                for (url, name) in map {
                    if let Some(abs) = absolutize(url, base) {
                        let name = name.as_str().map(String::from).unwrap_or_else(|| file_name_from_url(&abs));
                        out.push(RemoteAttachment { name, url: abs });
                    }
                }
            }
            Some(Value::Array(list)) => {
                for entry in list {
                    let url = ["src", "url", "file", "path", "link"].iter().find_map(|k| str_field(entry, k));
                    if let Some(abs) = url.and_then(|u| absolutize(u, base)) {
                        let name = ["name", "nazov", "filename", "title"]
                            .iter()
                            .find_map(|k| str_field(entry, k))
                            .map(String::from)
                            .unwrap_or_else(|| file_name_from_url(&abs));
                        out.push(RemoteAttachment { name, url: abs });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Links embedded in the description HTML (`href`/`src`). All of them are kept as
/// attachments; the downloader decides which hosts it may fetch.
fn collect_embedded_links(html: &str, base: &str, out: &mut Vec<RemoteAttachment>) {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"(?i)(?:href|src)\s*=\s*["']([^"']+)["']"#).unwrap());
    for cap in re.captures_iter(html) {
        let raw = html_unescape(&cap[1]);
        if let Some(abs) = absolutize(&raw, base) {
            out.push(RemoteAttachment { name: file_name_from_url(&abs), url: abs });
        }
    }
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">")
}

fn normalize_due(date: &str) -> Option<String> {
    let d = date.trim();
    if d.is_empty() || d.starts_with("0000") {
        return None;
    }
    if let Ok(nd) = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
        return Some(format!("{}T23:59:00", nd.format("%Y-%m-%d")));
    }
    if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(d, "%Y-%m-%d %H:%M:%S") {
        return Some(ndt.format("%Y-%m-%dT%H:%M:%S").to_string());
    }
    Some(d.to_string())
}

fn content_hash(title: &str, desc: &str, due: &Option<String>, atts: &[RemoteAttachment]) -> String {
    let mut h = Sha1::new();
    h.update(title.as_bytes());
    h.update([0]);
    h.update(desc.as_bytes());
    h.update([0]);
    h.update(due.as_deref().unwrap_or("").as_bytes());
    let mut urls: Vec<&str> = atts.iter().map(|a| a.url.as_str()).collect();
    urls.sort_unstable();
    for u in urls {
        h.update([0]);
        h.update(u.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Turn timeline items into assignments. Several timeline items can describe the
/// same homework (creation, later edits); they are grouped by `superid` and the
/// newest non-empty value of every field wins.
pub fn homework_from_items(items: &[Value], dbi: &Value, user_props: &Value, subdomain: &str) -> Vec<RemoteAssignmentRow> {
    let base = format!("https://{subdomain}.edupage.org/");
    let mut groups: BTreeMap<String, Vec<(&Value, Value)>> = BTreeMap::new();
    for item in items {
        if item.get("typ").and_then(Value::as_str) != Some("homework") {
            continue;
        }
        if item.get("removed").and_then(id_string).as_deref() == Some("1") {
            continue;
        }
        let data = item_data(item);
        let key = data
            .get("superid")
            .and_then(id_string)
            .or_else(|| item.get("ineid").and_then(id_string))
            .or_else(|| item.get("timelineid").and_then(id_string));
        if let Some(key) = key {
            groups.entry(key).or_default().push((item, data));
        }
    }

    let mut out = Vec::new();
    for (external_id, mut entries) in groups {
        // Newest first.
        entries.sort_by(|a, b| {
            let ta = a.0.get("timestamp").and_then(Value::as_str).unwrap_or("");
            let tb = b.0.get("timestamp").and_then(Value::as_str).unwrap_or("");
            tb.cmp(ta)
        });
        let pick = |f: &dyn Fn(&Value, &Value) -> Option<String>| entries.iter().find_map(|(i, d)| f(i, d));

        let title = pick(&|_, d| str_field(d, "nazov").or_else(|| str_field(d, "title")).map(String::from))
            .or_else(|| pick(&|i, _| str_field(i, "text").map(|t| t.lines().next().unwrap_or(t).to_string())))
            .unwrap_or_else(|| "Untitled assignment".into());
        let description_html = pick(&|_, d| str_field(d, "popis").map(String::from))
            .or_else(|| pick(&|i, _| str_field(i, "text").map(String::from)))
            .unwrap_or_default();
        let due_at = pick(&|_, d| str_field(d, "date").or_else(|| str_field(d, "datetime")).and_then(normalize_due))
            .or_else(|| pick(&|i, _| str_field(i, "cas_udalosti").and_then(normalize_due)));
        let subject = pick(&|_, d| d.get("predmetid").and_then(id_string).and_then(|id| subject_name(dbi, &id)));
        let teacher = pick(&|i, _| str_field(i, "vlastnik_meno").map(String::from));
        let teacher_ref = pick(&|i, _| str_field(i, "vlastnik").filter(|v| v.starts_with("Ucitel")).map(String::from))
            .or_else(|| teacher.as_deref().and_then(|t| teacher_ref_by_name(dbi, t)));
        let assigned_at = entries.last().and_then(|(i, _)| str_field(i, "cas_pridania").or_else(|| str_field(i, "timestamp")).map(String::from));
        let platform_done = entries.iter().any(|(i, _)| {
            i.get("timelineid")
                .and_then(id_string)
                .and_then(|tid| user_props.get(tid))
                .and_then(|p| str_field(p, "doneMaxCas"))
                .is_some()
        });

        let mut attachments = Vec::new();
        for (_, d) in &entries {
            collect_attachments(d, &base, &mut attachments);
        }
        collect_embedded_links(&description_html, &base, &mut attachments);
        let mut seen = HashMap::new();
        attachments.retain(|a| seen.insert(a.url.clone(), ()).is_none());

        let raw: Vec<Value> = entries.iter().map(|(i, _)| (*i).clone()).collect();
        out.push(RemoteAssignmentRow {
            platform: PLATFORM.into(),
            content_hash: content_hash(&title, &description_html, &due_at, &attachments),
            description_text: html_to_text(&description_html),
            external_id,
            title,
            subject,
            teacher,
            teacher_ref,
            description_html,
            due_at,
            assigned_at,
            raw_json: serde_json::to_string(&raw).unwrap_or_default(),
            platform_done,
            attachments,
        });
    }
    out
}

/// Is this URL one EduTask may download with the school session cookies?
pub fn is_school_host(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .filter(|u| u.scheme() == "https")
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .is_some_and(|h| h == "edupage.org" || h.ends_with(".edupage.org"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (Vec<Value>, Value, Value) {
        let data_v1 = json!({"nazov":"Kalkulačka","popis":"<p>Napíšte kalkulačku v C++. Zadanie: <a href=\"/elearning/?cmd=file&amp;id=9\">zadanie.pdf</a></p>","date":"2026-12-01","predmetid":"5","superid":"777"}).to_string();
        let data_v2 = json!({"nazov":"Kalkulačka v2","date":"2026-12-03","superid":"777",
            "attachements":{"/files/spec.docx":"spec.docx"}}).to_string();
        let items = vec![
            json!({"timelineid":"100","typ":"homework","timestamp":"2026-11-01 08:00:00","cas_pridania":"2026-11-01 08:00:00","text":"","data":data_v1,"vlastnik":"Ucitel42","vlastnik_meno":"Ján Novák"}),
            json!({"timelineid":"101","typ":"homework","timestamp":"2026-11-02 09:00:00","text":"","data":data_v2,"vlastnik_meno":"Ján Novák"}),
            json!({"timelineid":"102","typ":"sprava","timestamp":"2026-11-02 09:00:00","text":"hello","data":"{}"}),
            json!({"timelineid":"103","typ":"homework","timestamp":"2026-11-02 09:00:00","text":"Old","removed":"1","data":"{\"superid\":\"5\"}"}),
        ];
        let dbi = json!({"subjects":{"5":{"name":"Programovanie","short":"PRG"}},"teachers":{"42":{"firstname":"Ján","lastname":"Novák"}}});
        let props = json!({"101":{"doneMaxCas":"2026-11-03 10:00:00"}});
        (items, dbi, props)
    }

    #[test]
    fn extracts_userhome_json_with_raw_control_chars() {
        let html = "<script>var x=1; userhome({\"userid\":\"Student1\",\"items\":[{\"text\":\"line1\nline2\"}]});\nother();</script>";
        let v = extract_userhome(html).unwrap();
        assert_eq!(v["userid"], "Student1");
        assert_eq!(userhome_items(&v).len(), 1);
    }

    #[test]
    fn missing_userhome_means_not_authenticated() {
        assert!(matches!(extract_userhome("<html>login</html>"), Err(AppError::NotAuthenticated(_))));
    }

    #[test]
    fn merges_homework_versions_newest_wins() {
        let (items, dbi, props) = fixture();
        let hw = homework_from_items(&items, &dbi, &props, "gymba");
        assert_eq!(hw.len(), 1, "removed and non-homework items are skipped");
        let a = &hw[0];
        assert_eq!(a.external_id, "777");
        assert_eq!(a.title, "Kalkulačka v2");
        assert_eq!(a.due_at.as_deref(), Some("2026-12-03T23:59:00"));
        assert!(a.description_text.contains("kalkulačku"), "description falls back to older version");
        assert_eq!(a.subject.as_deref(), Some("Programovanie"));
        assert_eq!(a.teacher.as_deref(), Some("Ján Novák"));
        assert_eq!(a.teacher_ref.as_deref(), Some("Ucitel42"));
        assert!(a.platform_done);
        let urls: Vec<_> = a.attachments.iter().map(|x| x.url.as_str()).collect();
        assert!(urls.contains(&"https://gymba.edupage.org/files/spec.docx"));
        assert!(urls.contains(&"https://gymba.edupage.org/elearning/?cmd=file&id=9"));
    }

    #[test]
    fn hash_changes_when_content_changes() {
        let (mut items, dbi, props) = fixture();
        let h1 = homework_from_items(&items, &dbi, &props, "s")[0].content_hash.clone();
        items[1]["data"] = json!(json!({"nazov":"Kalkulačka v3","superid":"777"}).to_string());
        let h2 = homework_from_items(&items, &dbi, &props, "s")[0].content_hash.clone();
        assert_ne!(h1, h2);
    }

    #[test]
    fn teacher_ref_falls_back_to_dbi_lookup() {
        let dbi = json!({"teachers":{"9":{"firstname":"Eva","lastname":"Malá"}}});
        assert_eq!(teacher_ref_by_name(&dbi, "Malá Eva").as_deref(), Some("Ucitel9"));
    }

    #[test]
    fn school_host_check() {
        assert!(is_school_host("https://gymba.edupage.org/x"));
        assert!(is_school_host("https://cloud.edupage.org/x"));
        assert!(!is_school_host("https://edupage.org.evil.com/x"));
        assert!(!is_school_host("http://gymba.edupage.org/x"));
        assert!(!is_school_host("https://youtube.com/watch"));
    }
}
