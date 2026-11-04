//! The house Discord card, ported from the discord-cards kit (`card.mjs`): one Components V2
//! container with the tone's accent colour, a head with the heading, status, facts and fields,
//! a divider before each list, and a small footer with the time. No embeds, no mentions.
//!
//! [`Card`] serializes to the same JSON spec `card.mjs --json` reads, which is what the card
//! relay gets. [`Card::to_discord`] builds the webhook payload Tubarr posts itself.

use serde::Serialize;
use serde_json::{Value, json};

/// Discord's flag for a Components V2 message.
pub const IS_COMPONENTS_V2: u64 = 1 << 15;
/// Text across all text displays in one message.
const MAX_TEXT: usize = 4000;
/// Past this a list ends with "… and N more".
pub const PER_LIST: usize = 20;

/// How serious a card is. Sets the emoji and the accent colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Success,
    Failure,
    Warning,
    Info,
}

impl Tone {
    pub fn emoji(self) -> &'static str {
        match self {
            Tone::Success => "✅",
            Tone::Failure => "🔴",
            Tone::Warning => "🟠",
            Tone::Info => "🔵",
        }
    }

    pub fn accent(self) -> u32 {
        match self {
            Tone::Success => 0x22c55e,
            Tone::Failure => 0x7f1d1d,
            Tone::Warning => 0xf59e0b,
            Tone::Info => 0x94a3b8,
        }
    }
}

/// A fact shown as `**Name** · value`. The value is escaped when rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Field {
    pub name: String,
    pub value: String,
}

/// A section after a divider: a bold name and one line per item, items escaped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct List {
    pub name: String,
    pub items: Vec<String>,
}

/// One card. Field names follow `card.mjs`'s JSON spec, plus `source` and `event` for the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    pub source: &'static str,
    /// Which notice this is, e.g. `canary_failing`. Also the only thing logs say about a card.
    pub event: &'static str,
    pub tone: Tone,
    /// Ours, not escaped. The tone's emoji goes in front.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Sentences, escaped.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub text: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lists: Vec<List>,
    /// Who sent it, e.g. `Tubarr · canary`. The time is added after it.
    pub footer: String,
    /// Unix seconds, shown in each reader's own time zone.
    pub at: u64,
    pub username: &'static str,
}

impl Card {
    pub fn new(event: &'static str, tone: Tone, title: impl Into<String>, at: u64) -> Self {
        Self {
            source: "tubarr",
            event,
            tone,
            title: title.into(),
            status: None,
            text: Vec::new(),
            fields: Vec::new(),
            lists: Vec::new(),
            footer: "Tubarr".to_owned(),
            at,
            username: "Tubarr",
        }
    }

    pub fn status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text.push(text.into());
        self
    }

    pub fn field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push(Field {
            name: name.into(),
            value: value.into(),
        });
        self
    }

    pub fn list(mut self, name: impl Into<String>, items: Vec<String>) -> Self {
        self.lists.push(List {
            name: name.into(),
            items,
        });
        self
    }

    pub fn footer(mut self, part: &str) -> Self {
        self.footer = format!("Tubarr · {part}");
        self
    }

    /// The webhook payload: `card.mjs`'s `buildCard` for this spec.
    pub fn to_discord(&self) -> Value {
        let mut head = vec![format!(
            "### {} {}",
            self.tone.emoji(),
            clip(&self.title, 150)
        )];
        if let Some(status) = &self.status {
            head.push(format!("**{}**", md(status, 200)));
        }
        head.extend(self.text.iter().map(|t| md(t, 500)));
        if !self.fields.is_empty() {
            head.push(String::new());
        }
        head.extend(
            self.fields
                .iter()
                .map(|f| format!("**{}** · {}", md(&f.name, 60), md(&f.value, 300))),
        );
        let mut components = vec![text_display(clip(&head.join("\n"), 1200))];

        let budget = (MAX_TEXT - 1400) / self.lists.len().max(1);
        for list in &self.lists {
            let mut lines = vec![format!("**{}**", md(&list.name, 80))];
            lines.extend(
                list.items
                    .iter()
                    .take(PER_LIST)
                    .map(|i| format!("- {}", md(i, 500))),
            );
            if list.items.len() > PER_LIST {
                lines.push(format!("-# … and {} more", list.items.len() - PER_LIST));
            }
            components.push(divider());
            components.push(text_display(clip_lines(&lines.join("\n"), budget)));
        }
        components.push(text_display(format!(
            "-# {} · <t:{}:f>",
            md(&self.footer, 150),
            self.at
        )));
        json!({
            "username": self.username,
            "flags": IS_COMPONENTS_V2,
            "allowed_mentions": { "parse": [] },
            "components": [{
                "type": 17,
                "accent_color": self.tone.accent(),
                "components": components,
            }],
        })
    }
}

fn text_display(content: String) -> Value {
    json!({ "type": 10, "content": content })
}

fn divider() -> Value {
    json!({ "type": 14, "divider": true, "spacing": 1 })
}

/// One line of outside text, clipped, with every Markdown and mention character escaped.
pub fn md(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for c in clip(&one_line, max).chars() {
        if "\\*_~`|>#[]()<:@-".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `text` cut to `max` characters, ending in `…` when cut.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Cuts at a line boundary so no line is left half shown, and says so.
fn clip_lines(text: &str, max: usize) -> String {
    let len = |lines: &[&str]| lines.join("\n").chars().count();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    while lines.len() > 1 && len(&lines) > max - 20 {
        lines.pop();
    }
    format!("{}\n-# …", clip(&lines.join("\n"), max - 20))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(payload: &Value) -> Vec<String> {
        payload["components"][0]["components"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["content"].as_str().map(str::to_owned))
            .collect()
    }

    #[test]
    fn renders_the_house_shape() {
        let card = Card::new("test", Tone::Failure, "Something broke", 1_791_499_315)
            .status("3 failed checks in a row")
            .text("One sentence.")
            .field("Last error", "YouTube asked for a sign-in check")
            .list("Releases", vec!["a".to_owned(), "b".to_owned()])
            .footer("canary");
        let p = card.to_discord();
        assert_eq!(p["flags"], 32768);
        assert_eq!(p["username"], "Tubarr");
        assert_eq!(p["allowed_mentions"], json!({ "parse": [] }));
        assert!(p.get("embeds").is_none() && p.get("content").is_none());
        let container = &p["components"][0];
        assert_eq!(container["type"], 17);
        assert_eq!(container["accent_color"], 0x7f1d1d);
        let types: Vec<u64> = container["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["type"].as_u64().unwrap())
            .collect();
        assert_eq!(types, [10, 14, 10, 10]);
        let t = texts(&p);
        assert_eq!(
            t[0],
            "### 🔴 Something broke\n**3 failed checks in a row**\nOne sentence.\n\n**Last error** · YouTube asked for a sign\\-in check"
        );
        assert_eq!(t[1], "**Releases**\n- a\n- b");
        assert_eq!(t[2], "-# Tubarr · canary · <t:1791499315:f>");
    }

    #[test]
    fn outside_text_cannot_inject_links_or_mentions() {
        let card = Card::new("test", Tone::Info, "Grab", 0)
            .field("Release", "[x](https://evil) <@1> @everyone")
            .list("Releases", vec!["**bold**\nnext line".to_owned()]);
        let t = texts(&card.to_discord());
        assert!(
            t[0].ends_with("\\[x\\]\\(https\\://evil\\) \\<\\@1\\> \\@everyone"),
            "{}",
            t[0]
        );
        assert!(t[1].ends_with("- \\*\\*bold\\*\\* next line"), "{}", t[1]);
    }

    #[test]
    fn long_lists_and_text_stay_inside_the_limits() {
        let items: Vec<String> = (0..50)
            .map(|i| format!("{i} {}", "x".repeat(200)))
            .collect();
        let card = Card::new("test", Tone::Warning, "t".repeat(300), 0)
            .field("Error", "e".repeat(1000))
            .list("One", items.clone())
            .list("Two", items);
        let p = card.to_discord();
        let t = texts(&p);
        let total: usize = t.iter().map(|s| s.chars().count()).sum();
        assert!(total <= 4000, "{total}");
        assert!(t[0].chars().count() <= 1200);
        assert!(t[1].ends_with("-# …"), "{}", t[1]);
    }

    #[test]
    fn lists_past_the_cap_say_how_many_more() {
        let items: Vec<String> = (0..PER_LIST + 3).map(|i| i.to_string()).collect();
        let t = texts(
            &Card::new("test", Tone::Info, "t", 0)
                .list("L", items)
                .to_discord(),
        );
        assert!(t[1].ends_with("-# … and 3 more"), "{}", t[1]);
    }

    #[test]
    fn spec_json_matches_card_mjs_names() {
        let card = Card::new("grabs", Tone::Info, "t", 5).field("A", "b");
        let spec = serde_json::to_value(&card).unwrap();
        assert_eq!(spec["tone"], "info");
        assert_eq!(spec["fields"], json!([{ "name": "A", "value": "b" }]));
        assert_eq!(spec["at"], 5);
        assert_eq!(spec["source"], "tubarr");
        assert_eq!(spec["event"], "grabs");
        assert!(spec.get("lists").is_none() && spec.get("status").is_none());
    }
}
