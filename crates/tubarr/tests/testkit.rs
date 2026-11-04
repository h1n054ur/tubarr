//! Proves the test kit's fixtures parse and its fakes behave.

mod support;

use std::process::Command;

use support::*;

fn all_fixtures() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![fixtures_dir()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn json_fixtures_parse() {
    for path in all_fixtures()
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
    {
        let text = std::fs::read_to_string(path).unwrap();
        serde_json::from_str::<serde_json::Value>(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}

#[test]
fn xml_fixtures_parse() {
    for path in all_fixtures()
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "xml" || e == "nzb"))
    {
        let text = std::fs::read_to_string(path).unwrap();
        let options = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        };
        roxmltree::Document::parse_with_options(&text, options)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}

#[test]
fn fixtures_hold_no_ip_addresses() {
    for path in all_fixtures() {
        // Binary media fixtures can hold any byte run, so only text is scanned.
        let Ok(text) = String::from_utf8(std::fs::read(&path).unwrap()) else {
            continue;
        };
        let dotted = text
            .split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .any(|w| {
                let parts: Vec<&str> = w.split('.').collect();
                parts.len() == 4
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.parse::<u8>().is_ok())
            });
        assert!(!dotted, "{} holds an IPv4 address", path.display());
        assert!(
            !text.contains("ip="),
            "{} holds an ip= query",
            path.display()
        );
    }
}

#[test]
fn every_arr_request_has_its_files() {
    let requests = arr_requests();
    assert!(requests.iter().any(|r| r.app == "sonarr"));
    assert!(requests.iter().any(|r| r.app == "radarr"));
    for r in &requests {
        assert!(!r.response_bytes().is_empty(), "{}", r.name);
        if let Some((content_type, body)) = r.multipart_body() {
            assert!(content_type.starts_with("multipart/form-data"));
            assert!(body.windows(5).any(|w| w == b"<nzb "), "{}", r.name);
        }
    }
}

#[test]
fn arr_request_uri_matches_the_wire_format() {
    let r = arr_request("sonarr_episode_by_title");
    assert_eq!(
        r.uri(),
        "/api?t=tvsearch&cat=5040&extended=1&apikey=0123456789abcdef0123456789abcdef&offset=0&limit=100&q=Kabhi%20Main%20Kabhi%20Tum&season=1&ep=1"
    );
    assert_eq!(
        arr_request("sonarr_sab_addfile").param("mode"),
        Some("addfile")
    );
}

#[test]
fn fake_yt_dlp_replays_a_failure() {
    let dir = temp_dir();
    let fake = FakeYtDlp::from_index(dir.path(), "bot_check");
    let out = Command::new(&fake.path)
        .args(["-J", "--", "0eD-GQCutAU"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "null");
    assert!(String::from_utf8_lossy(&out.stderr).contains("confirm you\u{2019}re not a bot"));
    assert_eq!(fake.last_args(), ["-J", "--", "0eD-GQCutAU"]);
}

#[test]
fn fake_yt_dlp_prints_a_json_fixture() {
    let dir = temp_dir();
    let fake = FakeYtDlp::new(dir.path(), Some("arr/sabnzbd/version.json"), None, 0);
    let out = Command::new(&fake.path).output().unwrap();

    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["version"], "4.5.1");
    assert!(out.stderr.is_empty());
}
