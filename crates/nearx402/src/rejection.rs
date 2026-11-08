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

/// `https://user:key@host:port/path?query` becomes `https://host:port`, keeping
/// any characters before the scheme and closing punctuation after the URL.
fn strip_url(word: &str) -> String {
    let Some(sep) = word.find("://") else {
        return word.to_string();
    };
    let is_scheme_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-');
    let scheme_at = word[..sep]
        .rfind(|c: char| !is_scheme_char(c))
        .map(|j| j + 1)
        .unwrap_or(0);
    if scheme_at == sep {
        return word.to_string();
    }
    let rest = &word[sep + 3..];
    let end = rest
        .find(['/', '?', '#', ')', ']', '"', '\'', ','])
        .unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit('@').next().unwrap_or("");
    let tail: String = rest[end..]
        .chars()
        .rev()
        .take_while(|c| matches!(c, ')' | ']' | '"' | '\'' | ',' | '.' | ':' | ';'))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!(
        "{}{}://{host}{tail}",
        &word[..scheme_at],
        &word[scheme_at..sep]
    )
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
