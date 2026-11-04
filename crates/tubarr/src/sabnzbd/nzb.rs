//! Reading the NZB files Tubarr hands out, and turning their names into folder names.

/// Longest release name kept, in bytes. Leaves room for `.strm` and `.srt` suffixes under the
/// usual 255-byte file name limit.
const MAX_RELEASE_BYTES: usize = 200;

/// What an NZB from Tubarr's own indexer carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NzbInfo {
    /// The YouTube video id from `<meta type="tubarr-video">`, or a split upload's part ids joined
    /// by commas in play order.
    pub video_id: String,
    /// `<meta type="title">`, the release name the indexer gave it.
    pub title: Option<String>,
    /// `subject` of the first `<file>`.
    pub subject: Option<String>,
}

/// Why an uploaded file was refused. The text goes back to Sonarr and Radarr as SABnzbd's error.
#[derive(Debug, thiserror::Error)]
pub enum NzbError {
    #[error("invalid NZB file: {0}")]
    Xml(String),
    #[error("invalid NZB file: the root element is not <nzb>")]
    NotNzb,
    #[error("invalid NZB file: it holds no <file>")]
    NoFile,
    #[error("NZB file has no tubarr-video meta, so it did not come from Tubarr")]
    NoVideo,
    #[error("NZB file names an invalid YouTube video id")]
    BadVideo,
}

/// Parses an NZB and reads the video id Tubarr's indexer put in it.
pub fn parse(bytes: &[u8]) -> Result<NzbInfo, NzbError> {
    let text = std::str::from_utf8(bytes).map_err(|e| NzbError::Xml(e.to_string()))?;
    // Every NZB carries the newzBin DOCTYPE. roxmltree still guards against entity expansion.
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: 10_000,
        ..roxmltree::ParsingOptions::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, options)
        .map_err(|e| NzbError::Xml(e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "nzb" {
        return Err(NzbError::NotNzb);
    }
    let file = root
        .children()
        .find(|n| n.tag_name().name() == "file")
        .ok_or(NzbError::NoFile)?;
    let meta = |kind: &str| {
        root.descendants()
            .find(|n| n.tag_name().name() == "meta" && n.attribute("type") == Some(kind))
            .and_then(|n| n.text())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    let video_id = meta("tubarr-video").ok_or(NzbError::NoVideo)?;
    if crate::play::parts::parse(&video_id).is_err() {
        return Err(NzbError::BadVideo);
    }
    Ok(NzbInfo {
        video_id,
        title: meta("title"),
        subject: file.attribute("subject").map(str::to_owned),
    })
}

/// Picks the release name: the `nzbname` the arr sent, else the NZB's title, its file subject,
/// the upload's file name, and the video id as a last resort. The result is safe as a folder name.
pub fn release_name(nzbname: Option<&str>, info: &NzbInfo, upload_name: Option<&str>) -> String {
    let subject = info.subject.as_deref().map(|s| strip_suffix(s, ".strm"));
    let upload = upload_name.map(|s| strip_suffix(s, ".nzb"));
    [nzbname, info.title.as_deref(), subject, upload]
        .into_iter()
        .flatten()
        .map(sanitise)
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| info.video_id.clone())
}

fn strip_suffix<'a>(name: &'a str, suffix: &str) -> &'a str {
    let cut = name.len().saturating_sub(suffix.len());
    match name.get(cut..) {
        Some(end) if end.eq_ignore_ascii_case(suffix) => &name[..cut],
        _ => name,
    }
}

/// Drops characters no common file system takes (and path separators), trims leading and trailing
/// dots and spaces (so `.` and `..` vanish) and caps the length.
fn sanitise(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        .collect();
    let mut out = kept.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if out.len() > MAX_RELEASE_BYTES {
        let mut end = MAX_RELEASE_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out = out[..end].trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    }
    out.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nzb(head: &str, files: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><head>{head}</head>{files}</nzb>"#
        )
    }

    const FILE: &str = r#"<file poster="tubarr" date="0" subject="A.B.S01E01.strm"><groups><group>alt.binaries.tubarr</group></groups><segments><segment bytes="1" number="1">x@tubarr</segment></segments></file>"#;

    #[test]
    fn reads_video_title_and_subject() {
        let text = nzb(
            r#"<meta type="title">A.B.S01E01</meta><meta type="tubarr-video">0eD-GQCutAU</meta>"#,
            FILE,
        );
        let info = parse(text.as_bytes()).unwrap();
        assert_eq!(
            info,
            NzbInfo {
                video_id: "0eD-GQCutAU".into(),
                title: Some("A.B.S01E01".into()),
                subject: Some("A.B.S01E01.strm".into()),
            }
        );
    }

    #[test]
    fn reads_the_parts_of_a_joined_release() {
        let parts = r#"<meta type="tubarr-video">0eD-GQCutAU,ldmPWRb-XcM</meta>"#;
        let info = parse(nzb(parts, FILE).as_bytes()).unwrap();
        assert_eq!(info.video_id, "0eD-GQCutAU,ldmPWRb-XcM");
        let bad = r#"<meta type="tubarr-video">0eD-GQCutAU,../etc</meta>"#;
        assert!(matches!(
            parse(nzb(bad, FILE).as_bytes()),
            Err(NzbError::BadVideo)
        ));
    }

    #[test]
    fn refuses_what_is_not_a_tubarr_nzb() {
        let video = r#"<meta type="tubarr-video">0eD-GQCutAU</meta>"#;
        type Check = fn(&NzbError) -> bool;
        let cases: [(String, Check); 5] = [
            ("not xml at all".into(), |e| matches!(e, NzbError::Xml(_))),
            ("<rss></rss>".into(), |e| matches!(e, NzbError::NotNzb)),
            (nzb(video, ""), |e| matches!(e, NzbError::NoFile)),
            (nzb("", FILE), |e| matches!(e, NzbError::NoVideo)),
            (
                nzb(r#"<meta type="tubarr-video">../../etc</meta>"#, FILE),
                |e| matches!(e, NzbError::BadVideo),
            ),
        ];
        for (text, check) in cases {
            let err = parse(text.as_bytes()).unwrap_err();
            assert!(check(&err), "{text:?} gave {err:?}");
        }
    }

    fn info(title: Option<&str>, subject: Option<&str>) -> NzbInfo {
        NzbInfo {
            video_id: "0eD-GQCutAU".into(),
            title: title.map(Into::into),
            subject: subject.map(Into::into),
        }
    }

    #[test]
    fn release_name_prefers_nzbname_then_title_subject_upload() {
        let full = info(Some("Title"), Some("Subject.strm"));
        assert_eq!(release_name(Some("Given"), &full, Some("Up.nzb")), "Given");
        assert_eq!(release_name(None, &full, Some("Up.nzb")), "Title");
        assert_eq!(
            release_name(None, &info(None, Some("Subject.STRM")), Some("Up.nzb")),
            "Subject"
        );
        assert_eq!(release_name(None, &info(None, None), Some("Up.nzb")), "Up");
        assert_eq!(
            release_name(Some(".."), &info(None, None), None),
            "0eD-GQCutAU"
        );
    }

    #[test]
    fn sanitise_makes_a_safe_folder_name() {
        assert_eq!(sanitise("../a/b\\c:d*e?f\"g<h>i|j\n"), "abcdefghij");
        assert_eq!(sanitise(" .Theri.2016. "), "Theri.2016");
        let long = "é".repeat(150);
        let cut = sanitise(&long);
        assert!(cut.len() <= MAX_RELEASE_BYTES && cut.chars().all(|c| c == 'é'));
    }
}
