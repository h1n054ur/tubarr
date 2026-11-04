//! WebVTT, as YouTube serves it, to SubRip, which Sonarr, Radarr and Jellyfin all take as an
//! extra file.

/// One cue, times in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cue {
    start: u64,
    end: u64,
    lines: Vec<String>,
}

/// Converts WebVTT to SRT. Cue settings, styling and karaoke timing are dropped, `<i>`, `<b>` and
/// `<u>` are kept. `rolling` is for YouTube's speech-to-text captions: each of their cues repeats
/// the line shown before it, then a 10 ms cue repeats it once more, so lines the previous cue
/// already showed are left out. Uploaded subtitles are converted as they are, where two cues in a
/// row may well say the same thing.
pub fn to_srt(vtt: &str, rolling: bool) -> String {
    let mut cues = parse(vtt);
    if rolling {
        cues = drop_repeats(cues);
    }
    let mut out = String::new();
    for (n, cue) in cues.iter().filter(|c| !c.lines.is_empty()).enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            n + 1,
            timestamp(cue.start),
            timestamp(cue.end),
            cue.lines.join("\n")
        ));
    }
    out
}

fn parse(vtt: &str) -> Vec<Cue> {
    let text = vtt
        .strip_prefix('\u{feff}')
        .unwrap_or(vtt)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut cues = Vec::new();
    // Blocks are separated by blank lines. The header, NOTE, STYLE and REGION blocks have no
    // timing line and fall out here.
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block.lines().collect();
        let Some(at) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let Some((start, end)) = timing(lines[at]) else {
            continue;
        };
        if end < start {
            continue;
        }
        let lines = lines[at + 1..]
            .iter()
            .map(|l| clean(l))
            .filter(|l| !l.is_empty())
            .collect();
        cues.push(Cue { start, end, lines });
    }
    cues
}

/// `00:00:01.000 --> 00:00:04.000 align:start position:0%` to start and end.
fn timing(line: &str) -> Option<(u64, u64)> {
    let (start, rest) = line.split_once("-->")?;
    let end = rest.split_whitespace().next()?;
    Some((parse_time(start.trim())?, parse_time(end)?))
}

/// `hh:mm:ss.ttt` or `mm:ss.ttt` to milliseconds.
fn parse_time(text: &str) -> Option<u64> {
    let (clock, millis) = text.split_once(['.', ','])?;
    if millis.len() != 3 {
        return None;
    }
    let millis: u64 = number(millis)?;
    let parts: Vec<&str> = clock.split(':').collect();
    let (h, m, s) = match parts.as_slice() {
        [h, m, s] => (number(h)?, number(m)?, number(s)?),
        [m, s] => (0, number(m)?, number(s)?),
        _ => return None,
    };
    if m > 59 || s > 59 {
        return None;
    }
    Some(((h * 60 + m) * 60 + s) * 1000 + millis)
}

fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn timestamp(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// One text line without tags (bar the three SRT knows), timestamps and entities, its spaces
/// collapsed.
fn clean(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            // A lone `<` is text.
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let tag = &after[..close];
        if tag.is_empty() || tag.starts_with(char::is_whitespace) || tag.contains('<') {
            // `a < b`: not a tag either.
            out.push('<');
            rest = after;
            continue;
        }
        if matches!(tag, "i" | "/i" | "b" | "/b" | "u" | "/u") {
            out.push('<');
            out.push_str(tag);
            out.push('>');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    let decoded = decode_entities(&out);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&lrm;", "")
        .replace("&rlm;", "")
        // Last, so `&amp;lt;` stays `&lt;`.
        .replace("&amp;", "&")
}

/// Leaves out the leading lines of each cue that the cue before it showed.
fn drop_repeats(cues: Vec<Cue>) -> Vec<Cue> {
    let mut previous: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for cue in cues {
        let shown = cue.lines.clone();
        let fresh: Vec<String> = cue
            .lines
            .into_iter()
            .skip_while(|l| previous.contains(l))
            .collect();
        if !shown.is_empty() {
            previous = shown;
        }
        if !fresh.is_empty() {
            out.push(Cue {
                lines: fresh,
                ..cue
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like an uploaded track from YouTube's timedtext `fmt=vtt`.
    const MANUAL: &str = "\u{feff}WEBVTT\r\nKind: captions\r\nLanguage: ur\r\n\r\nSTYLE\r\n::cue { color: white }\r\n\r\nNOTE made by hand\r\n\r\n1\r\n00:00:01.500 --> 00:00:04.000 align:start position:10% line:85%\r\n<v Narrator>Kabhi Main <i>Kabhi</i> Tum</v>\r\n\r\n00:00:04.000 --> 00:00:06.250\r\nNo!\r\n\r\n00:00:06.250 --> 00:00:07.000\r\nNo!\r\n\r\n00:00:07.000 --> 00:00:09.010\r\n<c.colorE5E5E5>Tom &amp; Jerry &lt;3</c>\r\n&nbsp;second   line\r\n\r\n01:02:03.004 --> 01:02:05.000\r\nلمبا عرصہ\r\n";

    /// Shaped like YouTube's speech-to-text `fmt=vtt`: every cue repeats the previous line, then
    /// a 10 ms cue repeats it again, and new words carry karaoke timestamps.
    const AUTO: &str = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.000 --> 00:00:02.879 align:start position:0%\n \nhello<00:00:00.320><c> world</c><00:00:00.640><c> this</c>\n\n00:00:02.879 --> 00:00:02.889 align:start position:0%\nhello world this\n \n\n00:00:02.889 --> 00:00:05.120 align:start position:0%\nhello world this\nis<00:00:03.200><c> a</c><00:00:03.520><c> test</c>\n\n00:00:05.120 --> 00:00:05.130 align:start position:0%\nis a test\n \n\n00:00:05.130 --> 00:00:07.000 align:start position:0%\nis a test\n[Music]\n";

    #[test]
    fn manual_track_converts_cue_by_cue() {
        assert_eq!(
            to_srt(MANUAL, false),
            "1\n00:00:01,500 --> 00:00:04,000\nKabhi Main <i>Kabhi</i> Tum\n\n\
             2\n00:00:04,000 --> 00:00:06,250\nNo!\n\n\
             3\n00:00:06,250 --> 00:00:07,000\nNo!\n\n\
             4\n00:00:07,000 --> 00:00:09,010\nTom & Jerry <3\nsecond line\n\n\
             5\n01:02:03,004 --> 01:02:05,000\nلمبا عرصہ\n\n"
        );
    }

    #[test]
    fn auto_captions_lose_the_rolling_repeats_and_karaoke_timing() {
        assert_eq!(
            to_srt(AUTO, true),
            "1\n00:00:00,000 --> 00:00:02,879\nhello world this\n\n\
             2\n00:00:02,889 --> 00:00:05,120\nis a test\n\n\
             3\n00:00:05,130 --> 00:00:07,000\n[Music]\n\n"
        );
    }

    #[test]
    fn short_times_and_broken_cues() {
        let vtt = "WEBVTT\n\n01:02.500 --> 01:03.000\nshort form\n\n\
                   00:00:05.000 --> 00:00:04.000\nends before it starts\n\n\
                   00:00:xx.000 --> 00:00:09.000\nbad time\n\n\
                   00:00:10.000 --> 00:00:11.000\n\n\n\
                   00:00:12.000 --> 00:00:13.000\n<b>a < b</b>\n";
        assert_eq!(
            to_srt(vtt, false),
            "1\n00:01:02,500 --> 00:01:03,000\nshort form\n\n\
             2\n00:00:12,000 --> 00:00:13,000\n<b>a < b</b>\n\n"
        );
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert_eq!(to_srt("WEBVTT\n\n", true), "");
        assert_eq!(to_srt("", false), "");
    }
}
