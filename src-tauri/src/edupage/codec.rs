//! EduPage's request/response wrapping used by its RPC endpoints (e.g. the login
//! RPC and `timeline/?akcia=createItem`).
//!
//! Ported from the community `edupage-api` Python library (`compression.py`),
//! which mirrors EduPage's own `edubarUtils.js`:
//!
//! * request:  `eqap = "dz:" + base64(raw_deflate(form_urlencoded))`,
//!             `eqacs = sha1_hex(eqap)`, `eqaz = "1"`
//! * response: optional `eqz:` (payload) or `eqwd:` (error) prefix + base64.

use std::io::{Read, Write};

use base64::Engine;
use flate2::{read::DeflateDecoder, write::DeflateEncoder, Compression};
use sha1::{Digest, Sha1};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Percent-encoding equivalent to Python's `urllib.parse.quote(s)` (safe = "/").
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn encode_form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", quote(k), quote(v))).collect::<Vec<_>>().join("&")
}

fn raw_deflate(data: &[u8]) -> Vec<u8> {
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).expect("writing to Vec cannot fail");
    enc.finish().expect("finishing deflate into Vec cannot fail")
}

/// Build the `application/x-www-form-urlencoded` body EduPage's RPC endpoints expect.
pub fn encode_request_body(pairs: &[(&str, &str)]) -> String {
    let form = encode_form(pairs);
    let eqap = format!("dz:{}", B64.encode(raw_deflate(form.as_bytes())));
    let eqacs = hex(&Sha1::digest(eqap.as_bytes()));
    encode_form(&[("eqap", &eqap), ("eqacs", &eqacs), ("eqaz", "1")])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode an RPC response body to text. Unprefixed bodies are returned as-is.
pub fn decode_response(body: &str) -> Option<String> {
    let payload = if let Some(rest) = body.strip_prefix("eqz:") {
        rest
    } else if let Some(rest) = body.strip_prefix("eqwd:") {
        rest
    } else {
        return Some(body.to_string());
    };
    let cleaned: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = B64.decode(cleaned.trim_end_matches('=')).or_else(|_| B64.decode(&cleaned)).ok()?;
    // The community client treats the decoded bytes as the payload directly; be
    // tolerant and also accept a raw-deflated payload.
    let mut inflated = Vec::new();
    if DeflateDecoder::new(&bytes[..]).read_to_end(&mut inflated).is_ok() && !inflated.is_empty() {
        if let Ok(s) = String::from_utf8(inflated) {
            if s.trim_start().starts_with(['{', '[', '"']) {
                return Some(s);
            }
        }
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::DeflateDecoder;

    #[test]
    fn quote_matches_python_semantics() {
        assert_eq!(quote("a b/c?d=é"), "a%20b/c%3Fd%3D%C3%A9");
        assert_eq!(quote("{\"k\":1}"), "%7B%22k%22%3A1%7D");
    }

    #[test]
    fn request_body_round_trips_and_checksum_is_sha1_of_eqap() {
        let body = encode_request_body(&[("rpcparams", "{\"username\":\"jan\"}")]);
        let parts: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
        let eqap = &parts["eqap"];
        assert!(eqap.starts_with("dz:"));
        assert_eq!(parts["eqaz"], "1");
        assert_eq!(parts["eqacs"], hex(&Sha1::digest(eqap.as_bytes())));
        let compressed = B64.decode(&eqap[3..]).unwrap();
        let mut out = String::new();
        DeflateDecoder::new(&compressed[..]).read_to_string(&mut out).unwrap();
        assert_eq!(out, "rpcparams=%7B%22username%22%3A%22jan%22%7D");
    }

    #[test]
    fn decode_handles_plain_prefixed_and_deflated() {
        assert_eq!(decode_response("{\"a\":1}").unwrap(), "{\"a\":1}");
        let plain = format!("eqz:{}", B64.encode(b"{\"token\":\"x\"}"));
        assert_eq!(decode_response(&plain).unwrap(), "{\"token\":\"x\"}");
        let deflated = format!("eqz:{}", B64.encode(raw_deflate(b"{\"token\":\"y\"}")));
        assert_eq!(decode_response(&deflated).unwrap(), "{\"token\":\"y\"}");
    }
}
