//! The outbound proxy for YouTube traffic (`[network]`).
//!
//! When set, every request toward YouTube and Google's video hosts goes through it: yt-dlp gets
//! it in its environment, and the clients for feeds, channel pages, thumbnails and the `/play` relay are built
//! with it. Sonarr, Radarr, Jellyfin, notifications and the binary downloads never use it.
//!
//! reqwest also reads `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` from the process
//! environment for any client without its own proxy setting, and Docker can set those for every
//! container. So the direct clients (arrs, Jellyfin, sign-in, notifications, drafts lookups) call
//! `no_proxy()` and never follow them. The YouTube clients follow the environment only while
//! `[network]` is unset, as before the setting existed. The binary downloads from GitHub keep
//! following it, for sites where GitHub is reachable only through their own proxy.
//!
//! The URL can hold a user name and password, so the full value is reachable only through
//! [`OutboundProxy::expose`]. `Debug` and [`OutboundProxy::redacted`] show scheme, host and port.

use std::fmt;

use url::Url;

/// The schemes Tubarr and yt-dlp both understand.
pub const SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

/// A checked proxy URL.
#[derive(Clone, PartialEq, Eq)]
pub struct OutboundProxy {
    url: Url,
    /// `scheme://host:port`, worked out once.
    shown: String,
}

/// Why a proxy URL was refused. The messages never repeat the value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProxyError {
    #[error("is not a valid URL ({0})")]
    Malformed(String),
    #[error("scheme `{0}` is not supported (use http, https, socks5 or socks5h)")]
    Scheme(String),
    #[error("must name a host")]
    NoHost,
    #[error(
        "must be only scheme, optional user and password, host and port, with no path, query or fragment"
    )]
    Extra,
}

impl OutboundProxy {
    /// Checks `raw`, e.g. `socks5h://user:pass@10.0.0.2:1080`.
    pub fn parse(raw: &str) -> Result<Self, ProxyError> {
        // The url crate's messages describe the fault, never the input.
        let url = Url::parse(raw.trim()).map_err(|e| ProxyError::Malformed(e.to_string()))?;
        if !SCHEMES.contains(&url.scheme()) {
            // A scheme is never a secret, but cap it in case the input was something else.
            let scheme: String = url.scheme().chars().take(16).collect();
            return Err(ProxyError::Scheme(scheme));
        }
        let Some(host) = url.host_str().filter(|h| !h.is_empty()) else {
            return Err(ProxyError::NoHost);
        };
        if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
            return Err(ProxyError::Extra);
        }
        let port = url.port_or_known_default().unwrap_or(1080);
        let shown = format!("{}://{host}:{port}", url.scheme());
        Ok(Self { url, shown })
    }

    /// `scheme://host:port`, safe for logs, Health and error messages.
    pub fn redacted(&self) -> &str {
        &self.shown
    }

    /// The full URL with any credentials, for yt-dlp's environment and the HTTP clients. Never log
    /// the result.
    pub fn expose(&self) -> &str {
        // `Url` keeps a trailing `/` after the host, which yt-dlp and reqwest both accept.
        self.url.as_str()
    }

    /// The proxy for a reqwest client: every request the client sends goes through it.
    pub fn reqwest(&self) -> Result<reqwest::Proxy, reqwest::Error> {
        reqwest::Proxy::all(self.url.as_str())
    }

    /// `text` with the full URL and the credentials replaced by the redacted form, for messages
    /// that come from yt-dlp, which may quote its proxy when it cannot reach it.
    ///
    /// The secret is the password, or the user name when there is no password (token auth like
    /// `http://TOKEN@host`), each in its written and its percent-decoded form. From 8 characters
    /// up it is replaced wherever it appears. A shorter one is replaced only where it ends a URL's
    /// user part (right before `@`), so it cannot garble the words that sort a failure.
    pub fn scrub(&self, text: &str) -> String {
        let full = self.url.as_str();
        let mut out = text.replace(full, &self.shown);
        let bare = full.trim_end_matches('/');
        if bare != full {
            out = out.replace(bare, &self.shown);
        }
        let user = self.url.username();
        let password = self.url.password().filter(|p| !p.is_empty());
        let secret = match password {
            Some(password) => password,
            None if !user.is_empty() => user,
            None => return out,
        };
        // yt-dlp may print the decoded form of a percent-encoded user name or password.
        let decoded = |s: &str| percent_decode(s).unwrap_or_else(|_| s.to_owned());
        if let Some(password) = password {
            for (user, password) in [
                (user.to_owned(), password.to_owned()),
                (decoded(user), decoded(password)),
            ] {
                out = out.replace(&format!("{user}:{password}@"), "***@");
            }
        }
        let mut secrets = vec![secret.to_owned(), decoded(secret)];
        secrets.dedup();
        for secret in secrets {
            if secret.chars().count() >= 8 {
                out = out.replace(&secret, "***");
            } else {
                out = scrub_userinfo_end(&out, &secret);
            }
        }
        out
    }
}

/// Replaces `secret` where it ends a URL's user part: right before `@`, after the start of
/// `text` or a character that cannot be part of a user name or password (`:` and `/` included).
fn scrub_userinfo_end(text: &str, secret: &str) -> String {
    let pattern = format!(
        r"(^|[^A-Za-z0-9\-._~%!$&'()*+,;=]){}@",
        regex::escape(secret)
    );
    match regex::Regex::new(&pattern) {
        Ok(re) => re.replace_all(text, "${1}***@").into_owned(),
        // The pattern is escaped, so this never happens. Hide every copy rather than none.
        Err(_) => text.replace(secret, "***"),
    }
}

/// Percent-decodes `s`, failing on a result that is not UTF-8.
fn percent_decode(s: &str) -> Result<String, std::string::FromUtf8Error> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(byte) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out)
}

impl fmt::Debug for OutboundProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OutboundProxy({})", self.shown)
    }
}

/// Adds `proxy` to a client meant for YouTube. Without one the builder is left as it is.
pub fn youtube_client(
    builder: reqwest::ClientBuilder,
    proxy: Option<&OutboundProxy>,
) -> Result<reqwest::ClientBuilder, reqwest::Error> {
    Ok(match proxy {
        Some(proxy) => builder.proxy(proxy.reqwest()?),
        None => builder,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_four_schemes() {
        for (raw, shown) in [
            ("http://proxy:3128", "http://proxy:3128"),
            ("https://proxy.example", "https://proxy.example:443"),
            ("socks5://10.0.0.2:1080", "socks5://10.0.0.2:1080"),
            ("socks5h://gw", "socks5h://gw:1080"),
        ] {
            let proxy = OutboundProxy::parse(raw).unwrap();
            assert_eq!(proxy.redacted(), shown, "{raw}");
        }
    }

    #[test]
    fn refuses_other_schemes_and_shapes() {
        assert_eq!(
            OutboundProxy::parse("ftp://proxy:21"),
            Err(ProxyError::Scheme("ftp".into()))
        );
        assert_eq!(
            OutboundProxy::parse("socks4://proxy:1080"),
            Err(ProxyError::Scheme("socks4".into()))
        );
        assert!(matches!(
            OutboundProxy::parse("not a url"),
            Err(ProxyError::Malformed(_))
        ));
        assert_eq!(
            OutboundProxy::parse("http://proxy:3128/path"),
            Err(ProxyError::Extra)
        );
        assert_eq!(
            OutboundProxy::parse("http://proxy:3128/?a=b"),
            Err(ProxyError::Extra)
        );
    }

    #[test]
    fn credentials_never_show() {
        let proxy = OutboundProxy::parse("socks5h://alice:s3cr%40tpass@10.0.0.2:1080").unwrap();
        assert_eq!(proxy.redacted(), "socks5h://10.0.0.2:1080");
        let debug = format!("{proxy:?}");
        assert_eq!(debug, "OutboundProxy(socks5h://10.0.0.2:1080)");
        assert!(!debug.contains("alice") && !debug.contains("s3cr"));
        assert!(proxy.expose().contains("alice:s3cr%40tpass@"));

        let message = format!(
            "ERROR: Unable to connect to proxy {} (alice:s3cr@tpass@10.0.0.2, s3cr@tpass)",
            proxy.expose().trim_end_matches('/')
        );
        let scrubbed = proxy.scrub(&message);
        assert!(!scrubbed.contains("s3cr"), "{scrubbed}");
        assert!(!scrubbed.contains("alice"), "{scrubbed}");
        assert!(scrubbed.contains("socks5h://10.0.0.2:1080"), "{scrubbed}");
    }

    #[test]
    fn a_short_password_leaves_the_rest_of_a_message_alone() {
        let proxy = OutboundProxy::parse("http://u:o@proxy:3128").unwrap();
        let message = "ERROR: Sign in to confirm you're not a bot (via http://u:o@proxy:3128)";
        assert_eq!(
            proxy.scrub(message),
            "ERROR: Sign in to confirm you're not a bot (via http://proxy:3128)"
        );
    }

    #[test]
    fn a_user_only_token_is_hidden_in_any_form() {
        let proxy = OutboundProxy::parse("http://tok%2Fen12345@proxy:3128").unwrap();
        for message in [
            "Unable to connect to proxy http://tok%2Fen12345@proxy:3128/",
            "Unable to connect to proxy http://tok%2Fen12345@proxy:3128",
            "Unable to connect to proxy http://tok/en12345@proxy:3128",
            "Unable to connect to proxy tok%2Fen12345@proxy (auth tok/en12345 refused)",
            "Unable to connect to proxy 'http://tok%2Fen12345@proxy'",
        ] {
            let scrubbed = proxy.scrub(message);
            assert!(!scrubbed.contains("en12345"), "{scrubbed}");
        }

        let short = OutboundProxy::parse("socks5h://abc@gw:1080").unwrap();
        for message in [
            "proxy socks5h://abc@gw:1080 refused",
            "proxy socks5h://abc@gw refused",
            "proxy abc@gw:1080 refused",
            "proxy 'http://abc@gw' refused",
        ] {
            let scrubbed = short.scrub(message);
            assert!(!scrubbed.contains("abc"), "{scrubbed}");
        }
    }

    #[test]
    fn a_short_password_is_hidden_inside_urls_only() {
        let proxy = OutboundProxy::parse("http://alice:bot@proxy:3128").unwrap();
        let message = "ERROR: Sign in to confirm you're not a bot. Proxy http://alice:bot@proxy \
                       or bob:bot@other or http://x:bot@proxy:9";
        let scrubbed = proxy.scrub(message);
        assert!(
            scrubbed.starts_with("ERROR: Sign in to confirm you're not a bot. Proxy "),
            "{scrubbed}"
        );
        assert!(!scrubbed.contains(":bot@"), "{scrubbed}");
        assert!(!scrubbed.contains("alice"), "{scrubbed}");
        // A word that only happens to end in the password stays as it is.
        assert_eq!(proxy.scrub("robot@example"), "robot@example");
    }

    #[test]
    fn errors_never_repeat_the_value() {
        for raw in ["gopher://alice:hunter2@x", "http://alice:hunter2@x/p"] {
            let message = OutboundProxy::parse(raw).unwrap_err().to_string();
            assert!(!message.contains("hunter2"), "{message}");
            assert!(!message.contains("alice"), "{message}");
        }
    }

    #[test]
    fn the_client_builder_takes_it() {
        let proxy = OutboundProxy::parse("socks5h://u:p@127.0.0.1:1").unwrap();
        youtube_client(reqwest::Client::builder(), Some(&proxy))
            .unwrap()
            .build()
            .unwrap();
        youtube_client(reqwest::Client::builder(), None)
            .unwrap()
            .build()
            .unwrap();
    }
}
