//! Shared test kit: recorded fixtures and small fakes, so tests never touch the network.
//!
//! Each integration test file that needs it adds `mod support;`. Not every test uses every helper.
#![allow(dead_code)]

pub mod jellyfin;
pub mod mp4;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use tempfile::TempDir;

/// Root of `tests/fixtures`.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn fixture_path(rel: &str) -> PathBuf {
    fixtures_dir().join(rel)
}

pub fn fixture_bytes(rel: &str) -> Vec<u8> {
    let path = fixture_path(rel);
    fs::read(&path).unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()))
}

pub fn fixture_string(rel: &str) -> String {
    String::from_utf8(fixture_bytes(rel))
        .unwrap_or_else(|e| panic!("fixture {rel} is not UTF-8: {e}"))
}

pub fn fixture_json(rel: &str) -> serde_json::Value {
    serde_json::from_str(&fixture_string(rel))
        .unwrap_or_else(|e| panic!("fixture {rel} is not JSON: {e}"))
}

/// A fresh temporary directory, removed when dropped.
pub fn temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("tubarr-test-")
        .tempdir()
        .expect("creating temp dir")
}

/// A fake yt-dlp: an executable that prints canned stdout and stderr, exits with a set code
/// and records the arguments it was called with, one per line.
pub struct FakeYtDlp {
    pub path: PathBuf,
    args_log: PathBuf,
}

impl FakeYtDlp {
    /// Writes the fake into `dir`. `stdout` and `stderr` are fixture paths (relative to
    /// `tests/fixtures`) or `None` for no output.
    pub fn new(dir: &Path, stdout: Option<&str>, stderr: Option<&str>, exit_code: i32) -> Self {
        let path = dir.join("yt-dlp");
        let args_log = dir.join("yt-dlp.args");
        let cat = |rel: Option<&str>, redirect: &str| match rel {
            Some(rel) => format!("cat {}{redirect}\n", sh_quote(&fixture_path(rel))),
            None => String::new(),
        };
        let script = format!(
            "#!/bin/sh\n: > {log}\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {log}; done\n{out}{err}exit {exit_code}\n",
            log = sh_quote(&args_log),
            out = cat(stdout, ""),
            err = cat(stderr, " >&2"),
        );
        install_executable(&path, script.as_bytes());
        Self { path, args_log }
    }

    /// Builds the fake from an entry in `youtube/index.json`.
    pub fn from_index(dir: &Path, name: &str) -> Self {
        let index = fixture_json("youtube/index.json");
        let entry = index["fixtures"]
            .as_array()
            .and_then(|all| all.iter().find(|f| f["name"] == name))
            .unwrap_or_else(|| panic!("no youtube fixture named {name}"));
        let file = |key: &str| entry[key].as_str().map(|f| format!("youtube/{f}"));
        let exit_code = entry["exit_code"]
            .as_i64()
            .expect("fixture has no exit_code") as i32;
        Self::new(
            dir,
            file("stdout").as_deref(),
            file("stderr").as_deref(),
            exit_code,
        )
    }

    /// Arguments of the last call, or empty if it was never called.
    pub fn last_args(&self) -> Vec<String> {
        fs::read_to_string(&self.args_log)
            .map(|s| s.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

/// Puts an executable at `path`, replacing any file there.
///
/// The bytes go to a plain data file first and a child `install` process copies them into place,
/// so no thread of this test binary ever holds a write descriptor to the executable. If one did, a
/// sibling test forking at that moment would inherit it, and running the file would fail with
/// ETXTBSY ("Text file busy") until that child calls exec. Renaming does not help: the inherited
/// descriptor follows the inode.
pub fn install_executable(path: &Path, contents: &[u8]) {
    let mut source = path.as_os_str().to_owned();
    source.push(".content");
    let source = PathBuf::from(source);
    fs::write(&source, contents).expect("writing executable content");
    let status = Command::new("install")
        .arg("-m")
        .arg("755")
        .arg(&source)
        .arg(path)
        .status()
        .expect("running install");
    assert!(status.success(), "install {} failed", path.display());
    fs::remove_file(&source).expect("removing executable content");
}

fn sh_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// One recorded Sonarr or Radarr request from `arr/requests.json`.
#[derive(Debug, Deserialize)]
pub struct ArrRequest {
    pub name: String,
    pub app: String,
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub multipart: Option<Multipart>,
    /// Example good answer, relative to `arr/`.
    pub response: String,
}

#[derive(Debug, Deserialize)]
pub struct Multipart {
    pub field: String,
    pub filename: String,
    pub content_type: String,
    /// File body, relative to `arr/`.
    pub body: String,
}

#[derive(Deserialize)]
struct ArrRequests {
    requests: Vec<ArrRequest>,
}

impl ArrRequest {
    /// Path and query as the arr puts them on the wire.
    pub fn uri(&self) -> String {
        let query: Vec<String> = self
            .query
            .iter()
            .map(|(k, v)| format!("{k}={}", encode(v)))
            .collect();
        format!("{}?{}", self.path, query.join("&"))
    }

    pub fn param(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn response_bytes(&self) -> Vec<u8> {
        fixture_bytes(&format!("arr/{}", self.response))
    }

    /// The multipart/form-data body for an addfile request, with its content type header.
    pub fn multipart_body(&self) -> Option<(String, Vec<u8>)> {
        let part = self.multipart.as_ref()?;
        let boundary = "tubarr-test-boundary";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
            part.field, part.filename, part.content_type
        )
        .into_bytes();
        body.extend(fixture_bytes(&format!("arr/{}", part.body)));
        body.extend(format!("\r\n--{boundary}--\r\n").into_bytes());
        Some((format!("multipart/form-data; boundary={boundary}"), body))
    }
}

/// The placeholder API key every recorded request carries.
pub fn arr_api_key() -> String {
    fixture_json("arr/requests.json")["api_key"]
        .as_str()
        .expect("arr/requests.json has an api_key")
        .to_owned()
}

pub fn arr_requests() -> Vec<ArrRequest> {
    let parsed: ArrRequests =
        serde_json::from_str(&fixture_string("arr/requests.json")).expect("arr/requests.json");
    parsed.requests
}

pub fn arr_request(name: &str) -> ArrRequest {
    arr_requests()
        .into_iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no recorded arr request named {name}"))
}

// Percent-encodes like .NET Uri.EscapeDataString, which both arrs use for query values.
fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
