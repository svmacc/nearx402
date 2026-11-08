//! Why a vendor refused a claim, as one short line that is safe to print.
//!
//! A vendor answers a refused claim with `402` and puts the facilitator's reason
//! in `PaymentRequired.error`. That reason is untrusted text, and can name an RPC
//! URL, which may carry an API key. So the line here is bounded, has its control
//! characters removed, keeps only the scheme and host of any URL, and hides every
//! run of 64 hex digits (the length of a receipt secret, and of a nonce) and any
//! secret the caller names. It never includes the request's headers.

use std::fmt;

use crate::x402::PaymentRequired;

/// The longest detail kept, in characters.
pub const MAX_DETAIL: usize = 240;

/// The kind of refusal, from the reason's wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The facilitator's RPC answered 429. Says nothing about the payment.
    RpcRateLimited,
    /// The facilitator could not read the chain for another reason.
    RpcUnavailable,
    /// The vendor could not reach its facilitator.
    FacilitatorUnavailable,
    NonceExpired,
    NonceInvalid,
    AlreadyRedeemed,
    /// The memo on chain does not commit to the secret in the claim.
    SecretMismatch,
    NotPaid,
    TooOld,
    /// Another check on the payment failed.
    PaymentMismatch,
    /// No x402 body, or no reason in it.
    NoReason,
    Other,
}

impl Cause {
    pub fn label(self) -> &'static str {
        match self {
            Cause::RpcRateLimited => "the facilitator's RPC is rate limiting (429)",
            Cause::RpcUnavailable => "the facilitator could not read the chain",
            Cause::FacilitatorUnavailable => "the vendor could not reach its facilitator",
            Cause::NonceExpired => "the nonce expired",
            Cause::NonceInvalid => "the nonce is not the vendor's",
            Cause::AlreadyRedeemed => "the payment was already redeemed",
            Cause::SecretMismatch => "the receipt secret does not match the on-chain memo",
            Cause::NotPaid => "the log entry is not Paid",
            Cause::TooOld => "the payment is too old",
            Cause::PaymentMismatch => "the payment does not match the request",
            Cause::NoReason => "the vendor gave no reason",
            Cause::Other => "other",
        }
    }

    /// True if the refusal says something about the payment itself, rather
    /// than about an RPC or the facilitator being reachable.
    pub fn is_about_the_payment(self) -> bool {
        !matches!(
            self,
            Cause::RpcRateLimited
                | Cause::RpcUnavailable
                | Cause::FacilitatorUnavailable
                | Cause::NoReason
                | Cause::Other
        )
    }
}

/// A refused claim, ready to print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub status: u16,
    pub cause: Cause,
    /// Bounded and redacted. Empty if the vendor gave no reason.
    pub detail: String,
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP {}, {}", self.status, self.cause.label())?;
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

/// Reads a vendor's refusal. `secrets` are hidden wherever they appear.
pub fn explain(status: u16, body: &str, secrets: &[&str]) -> Rejection {
    let reason = serde_json::from_str::<PaymentRequired>(body)
        .ok()
        .and_then(|p| p.error)
        .unwrap_or_default();
    let detail = redact(&reason, secrets);
    let cause = if detail.is_empty() {
        Cause::NoReason
    } else {
        classify(&reason)
    };
    Rejection {
        status,
        cause,
        detail,
    }
}

/// The cause, from the reason before redaction. Order matters: an RPC failure
/// while reading the payment transaction is about the RPC, not the payment.
pub fn classify(reason: &str) -> Cause {
    let r = reason.to_ascii_lowercase();
    let has = |s: &str| r.contains(s);
    if has("too many requests") || has("status: 429") || has("rate limit") {
        Cause::RpcRateLimited
    } else if has("facilitator /") || has("facilitator unreachable") {
        Cause::FacilitatorUnavailable
    } else if has("expired nonce") {
        Cause::NonceExpired
    } else if has("invalid nonce") || has("different nonce") {
        Cause::NonceInvalid
    } else if has("already redeemed") {
        Cause::AlreadyRedeemed
    } else if has("does not carry this nonce and receipt secret") {
        Cause::SecretMismatch
    } else if has("not paid") || has("no such policy log entry") {
        Cause::NotPaid
    } else if has("can no longer be redeemed") {
        Cause::TooOld
    } else if has("rpc error")
        || has("rpc response")
        || has("error sending request")
        || has("timed out")
        || has("connection")
        || has("server error")
        || has("read payment transaction")
        || has("transporterror")
        || has("query")
    {
        Cause::RpcUnavailable
    } else if has("payment went to")
        || has("but the price is")
        || has("has no successful")
        || has("trusted policy code")
        || has("different token")
        || has("unsupported")
        || has("does not trust")
        || has("receipt secret must")
    {
        Cause::PaymentMismatch
    } else {
        Cause::Other
    }
}

/// One line, at most `MAX_DETAIL` characters, with secrets, 64-hex runs and the
/// path, query and credentials of URLs removed.
pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut s = text.to_string();
    for secret in secrets.iter().filter(|x| !x.is_empty()) {
        s = s.replace(secret, "[secret]");
    }
    let s: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let words: Vec<String> = s
        .split_whitespace()
        .map(|w| hide_hex_runs(&strip_url(w)))
        .collect();
    let line = words.join(" ");
    if line.chars().count() > MAX_DETAIL {
        format!("{}...", line.chars().take(MAX_DETAIL).collect::<String>())
    } else {
        line
    }
}

/// A URL as it may be printed or recorded: scheme and host only. An RPC URL
/// can carry an API key in its path, query or credentials.
pub fn public_url(url: &str) -> String {
    strip_url(url.trim())
}

/// Punctuation that may close a URL in running text, and is kept after it.
const CLOSING: [char; 8] = [')', ']', '"', '\'', ',', '.', ':', ';'];
/// Punctuation that may open the next URL in the same word.
const OPENING: [char; 3] = ['(', '[', '<'];

/// The run of characters at the end of `s` that `keep` accepts.
fn trailing(s: &str, keep: impl Fn(&char) -> bool) -> String {
    s.chars()
        .rev()
        .take_while(keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Every `https://user:key@host:port/path?query` in `word` becomes
/// `https://host:port`, keeping what comes before the scheme and closing
/// punctuation after the URL. `word` is untrusted text, so every cut is made
/// on a character boundary, never on a byte offset into a character.
fn strip_url(word: &str) -> String {
    let is_scheme_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-');
    // Where the scheme of the `://` at byte `sep` starts in `s`: right after
    // the last character before it that cannot be part of a scheme.
    let scheme_start = |s: &str, sep: usize| {
        s[..sep]
            .char_indices()
            .rev()
            .find(|(_, c)| !is_scheme_char(*c))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0)
    };
    let mut out = String::new();
    let mut rest = word;
    while let Some(sep) = rest.find("://") {
        if scheme_start(rest, sep) == sep {
            // `://` without a scheme is not a URL.
            out.push_str(&rest[..sep + 3]);
            rest = &rest[sep + 3..];
            continue;
        }
        let after = &rest[sep + 3..];
        let end = after
            .find(['/', '?', '#', ')', ']', '"', '\'', ','])
            .unwrap_or(after.len());
        let host = after[..end].rsplit('@').next().unwrap_or("");
        out.push_str(&rest[..sep]);
        out.push_str("://");
        out.push_str(host);
        let dropped = &after[end..];
        match dropped.find("://") {
            // Another URL in the same word: continue from its scheme, keeping
            // only the punctuation between the two, such as `)(` or `,`.
            Some(next) => {
                let at = scheme_start(dropped, next);
                out.push_str(&trailing(&dropped[..at], |c| {
                    CLOSING.contains(c) || OPENING.contains(c)
                }));
                rest = &dropped[at..];
            }
            None => {
                out.push_str(&trailing(dropped, |c| CLOSING.contains(c)));
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Replaces every run of exactly 64 or more hex digits.
fn hide_hex_runs(word: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 64 {
            out.push_str("[hex]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in word.chars() {
        if c.is_ascii_hexdigit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::{self, PaymentRequired};

    fn body(error: Option<&str>) -> String {
        serde_json::to_string(&PaymentRequired {
            x402_version: x402::VERSION,
            error: error.map(str::to_string),
            accepts: vec![],
        })
        .unwrap()
    }

    #[test]
    fn the_facilitators_own_reasons_map_to_causes() {
        let cases = [
            ("policy log entry does not carry this nonce and receipt secret", Cause::SecretMismatch),
            ("payment already redeemed", Cause::AlreadyRedeemed),
            ("expired nonce, request the resource again for a new one", Cause::NonceExpired),
            ("invalid nonce (not issued by this vendor for this resource and price)", Cause::NonceInvalid),
            ("payment is for a different nonce", Cause::NonceInvalid),
            ("policy log entry is Pending, not Paid", Cause::NotPaid),
            ("no such policy log entry", Cause::NotPaid),
            ("payment is older than 600 s and can no longer be redeemed", Cause::TooOld),
            ("payment went to a.testnet, not b.testnet", Cause::PaymentMismatch),
            ("payer contract does not run the trusted policy code", Cause::PaymentMismatch),
            ("read payment transaction: HTTP status client error (429 Too Many Requests) for url (https://rpc.testnet.near.org/)", Cause::RpcRateLimited),
            ("read payment transaction: RPC error: {\"code\":-32000}", Cause::RpcUnavailable),
            ("facilitator /verify returned 500 Internal Server Error: boom", Cause::FacilitatorUnavailable),
            ("something new", Cause::Other),
        ];
        for (reason, cause) in cases {
            let r = explain(402, &body(Some(reason)), &[]);
            assert_eq!(r.cause, cause, "{reason}");
            assert_eq!(r.status, 402);
        }
    }

    #[test]
    fn only_refusals_about_the_payment_say_something_about_it() {
        assert!(Cause::SecretMismatch.is_about_the_payment());
        assert!(Cause::AlreadyRedeemed.is_about_the_payment());
        for c in [
            Cause::RpcRateLimited,
            Cause::RpcUnavailable,
            Cause::FacilitatorUnavailable,
            Cause::NoReason,
            Cause::Other,
        ] {
            assert!(!c.is_about_the_payment(), "{c:?}");
        }
    }

    #[test]
    fn no_body_or_no_reason_is_said_plainly() {
        for b in ["", "not json", &body(None), &body(Some("   \n "))] {
            let r = explain(402, b, &[]);
            assert_eq!(r.cause, Cause::NoReason, "{b}");
            assert_eq!(r.detail, "");
            assert_eq!(r.to_string(), "HTTP 402, the vendor gave no reason");
        }
    }

    #[test]
    fn secrets_hex_runs_and_url_credentials_are_hidden() {
        let secret = "ab".repeat(32);
        let reason = format!(
            "secret {secret} and nonce {} via https://user:pw@rpc.example.com:8443/v1/key/abc?apiKey=SECRETKEY#x, (http://10.0.0.1/path).",
            "0".repeat(64)
        );
        let r = explain(402, &body(Some(&reason)), &[&secret]);
        for leak in [
            secret.as_str(),
            "user",
            "pw",
            "apiKey",
            "SECRETKEY",
            "/v1/key",
            "/path",
            &"0".repeat(64),
        ] {
            assert!(!r.detail.contains(leak), "{leak} leaked: {}", r.detail);
        }
        assert!(r.detail.contains("[secret]"), "{}", r.detail);
        assert!(
            r.detail.contains("https://rpc.example.com:8443"),
            "{}",
            r.detail
        );
        assert!(r.detail.contains("(http://10.0.0.1)."), "{}", r.detail);
        assert!(r.detail.contains("nonce [hex]"), "{}", r.detail);
    }

    #[test]
    fn a_public_url_keeps_only_its_scheme_and_host() {
        assert_eq!(
            public_url("https://user:pw@rpc.example.com:8443/v1/key/abc?apiKey=SECRET"),
            "https://rpc.example.com:8443"
        );
        assert_eq!(
            public_url("https://rpc.testnet.near.org"),
            "https://rpc.testnet.near.org"
        );
        assert_eq!(
            public_url("http://127.0.0.1:3030/"),
            "http://127.0.0.1:3030"
        );
        assert_eq!(public_url("not a url"), "not a url");
    }

    /// Codex's finding on `0ea1964`: a multibyte character right before a URL
    /// made the cut land inside that character, and the slice panicked.
    #[test]
    fn a_multibyte_character_before_a_url_does_not_panic() {
        assert_eq!(
            redact("error: éhttps://user:pw@rpc.example/path?key=SECRET", &[]),
            "error: éhttps://rpc.example"
        );
    }

    #[test]
    fn urls_next_to_multibyte_text_are_still_cut_to_their_host() {
        // Characters of 2, 3 and 4 bytes before, inside and after the URL.
        for prefix in ["", "é", "€", "😀", "日本", "(é", "x", "€:"] {
            for suffix in ["", "é", "€)", "😀.", "…", ")", "日"] {
                let s = format!(
                    "{prefix}https://us€r:pwé@rpc.example:8443/pä/th?apiKey=SECRET😀#f{suffix}"
                );
                let r = redact(&s, &[]);
                for leak in ["SECRET", "pw", "us€r", "/pä", "apiKey", "#f"] {
                    assert!(!r.contains(leak), "{s:?} -> {r:?} holds {leak}");
                }
                assert!(r.contains("https://rpc.example:8443"), "{s:?} -> {r:?}");
                assert!(r.starts_with(prefix), "{s:?} -> {r:?}");
            }
        }
    }

    #[test]
    fn every_url_in_a_word_is_cut_to_its_host() {
        assert_eq!(
            redact(
                "https://a.example/x?k=S1,https://u:p@b.example:8443/y#S2 then http://c.example/z.",
                &[]
            ),
            "https://a.example,https://b.example:8443 then http://c.example."
        );
        assert_eq!(
            redact("(https://a.example/x?k=S1)(https://b.example/y)", &[]),
            "(https://a.example)(https://b.example)"
        );
    }

    #[test]
    fn closing_punctuation_after_a_url_is_kept() {
        assert_eq!(
            redact("(see https://a.example/p?k=S).", &[]),
            "(see https://a.example)."
        );
        assert_eq!(
            redact("'https://a.example/p',", &[]),
            "'https://a.example',"
        );
        // No path, and `://` without a scheme: nothing to cut.
        assert_eq!(redact("a://b and ://c/d", &[]), "a://b and ://c/d");
    }

    #[test]
    fn short_hex_such_as_a_log_id_or_amount_is_kept() {
        let r = explain(
            402,
            &body(Some("paid 50000 but the price is 60000, log 3")),
            &[],
        );
        assert_eq!(r.detail, "paid 50000 but the price is 60000, log 3");
    }

    #[test]
    fn the_detail_is_one_bounded_line() {
        let long = format!("line one\nline two\r\t{}", "x".repeat(1000));
        let r = explain(402, &body(Some(&long)), &[]);
        assert!(!r.detail.contains(['\n', '\r', '\t']));
        assert!(r.detail.chars().count() <= MAX_DETAIL + 3);
        assert!(r.detail.starts_with("line one line two "));
        // Multi-byte characters are cut on a character boundary.
        let r = explain(402, &body(Some(&"é".repeat(1000))), &[]);
        assert_eq!(r.detail.chars().count(), MAX_DETAIL + 3);
    }
}
