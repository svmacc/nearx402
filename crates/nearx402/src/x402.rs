//! x402 wire types for the `near-policy` scheme.
//!
//! Follows the x402 v1 shape (402 body with `accepts`, `X-PAYMENT` request
//! header, `X-PAYMENT-RESPONSE` response header). The scheme is our own: the
//! payer settles through a nearx402 policy contract on NEAR and proves it with
//! the contract's log entry id, which the vendor checks on chain.

use anyhow::{ensure, Context, Result};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const VERSION: u32 = 1;
pub const SCHEME: &str = "near-policy";
pub const PAYMENT_HEADER: &str = "X-PAYMENT";
pub const PAYMENT_RESPONSE_HEADER: &str = "X-PAYMENT-RESPONSE";

/// Body of a `402 Payment Required` response.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequired {
    pub x402_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub accepts: Vec<PaymentRequirements>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirements {
    pub scheme: String,
    /// For example `near-sandbox` or `near-testnet`.
    pub network: String,
    /// Price in token base units, as a decimal string.
    pub max_amount_required: String,
    pub resource: String,
    pub description: String,
    pub mime_type: String,
    /// Vendor NEAR account that must receive the tokens.
    pub pay_to: String,
    /// NEP-141 token contract.
    pub asset: String,
    pub max_timeout_seconds: u64,
    pub extra: RequirementsExtra,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementsExtra {
    /// One-time value the payer must put in the policy log memo.
    pub nonce: String,
    /// Policy contract code hash (base58) this vendor accepts payments from.
    pub policy_code_hash: String,
}

/// Decoded `X-PAYMENT` header.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    pub x402_version: u32,
    pub scheme: String,
    pub network: String,
    pub payload: NearPolicyProof,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NearPolicyProof {
    pub policy_contract: String,
    pub log_id: u32,
    pub nonce: String,
    /// Hash of the agent's `pay` transaction, for explorers and receipts.
    pub tx_hash: String,
    /// Opens the commitment in the log entry's memo. Everything else in this
    /// proof is public once the payment is on chain, so this is what makes the
    /// payer the only one able to claim it. See [`payment_memo`].
    pub receipt_secret: String,
}

/// The longest nonce that fits the contract's 128-byte memo, with the 44 bytes
/// that [`payment_memo`] adds.
pub const MAX_NONCE_LEN: usize = 84;

/// A new secret for one payment: 256 random bits, as 64 lowercase hex digits.
pub fn new_receipt_secret() -> String {
    rand::random::<[u8; 32]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the payer puts in the policy log's memo: the vendor's nonce and a
/// commitment to a secret, `nonce:base64url(sha256(domain, secret))`. The memo is
/// public, and the secret stays with the payer until it claims the resource, so
/// reading the chain is not enough to claim it. A 64-character nonce gives 108
/// bytes, within the contract's limit of 128.
pub fn payment_memo(nonce: &str, receipt_secret: &str) -> Result<String> {
    // One spelling only, so that one secret has one commitment.
    ensure!(
        receipt_secret.len() == 64
            && receipt_secret
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "receipt secret must be 64 lowercase hex digits"
    );
    let commitment = Sha256::new()
        .chain_update(b"nearx402-receipt-v1\n")
        .chain_update(receipt_secret.as_bytes())
        .finalize();
    Ok(format!("{nonce}:{}", URL_SAFE_NO_PAD.encode(commitment)))
}

/// Facilitator `POST /settle` response, also sent to the client base64-encoded
/// in the `X-PAYMENT-RESPONSE` header.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementResponse {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    pub network: String,
    pub transaction: String,
    pub payer: String,
}

/// Body of facilitator `POST /verify` and `POST /settle` requests.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacilitatorRequest {
    pub x402_version: u32,
    pub payment_payload: PaymentPayload,
    pub payment_requirements: PaymentRequirements,
}

/// Facilitator `POST /verify` response.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    pub is_valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
}

/// Facilitator `GET /supported` response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Supported {
    pub kinds: Vec<SupportedKind>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedKind {
    pub x402_version: u32,
    pub scheme: String,
    pub network: String,
    pub extra: SupportedExtra,
}

/// What a vendor needs to put in its `402` requirements for this facilitator.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedExtra {
    /// NEP-141 token contract the facilitator accepts.
    pub asset: String,
    /// Policy contract code hash (base58) the facilitator trusts.
    pub policy_code_hash: String,
}

pub fn encode<T: Serialize>(value: &T) -> Result<String> {
    Ok(B64.encode(serde_json::to_vec(value)?))
}

pub fn decode<T: for<'de> Deserialize<'de>>(header: &str) -> Result<T> {
    let bytes = B64.decode(header.trim()).context("header is not base64")?;
    serde_json::from_slice(&bytes).context("header is not valid x402 JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payment_header_round_trips() {
        let p = PaymentPayload {
            x402_version: VERSION,
            scheme: SCHEME.into(),
            network: "near-sandbox".into(),
            payload: NearPolicyProof {
                policy_contract: "policy.sandbox".into(),
                log_id: 7,
                nonce: "abc".into(),
                tx_hash: "hash".into(),
                receipt_secret: new_receipt_secret(),
            },
        };
        let back: PaymentPayload = decode(&encode(&p).unwrap()).unwrap();
        assert_eq!(back.payload.log_id, 7);
        assert_eq!(back.payload.policy_contract, "policy.sandbox");
        assert_eq!(back.payload.receipt_secret, p.payload.receipt_secret);
    }

    #[test]
    fn a_receipt_memo_commits_to_its_secret_and_fits_the_contract() {
        let secret = new_receipt_secret();
        assert_eq!(secret.len(), 64);
        assert_ne!(secret, new_receipt_secret());

        let nonce = "n".repeat(64);
        let memo = payment_memo(&nonce, &secret).unwrap();
        assert_eq!(memo.len(), 108);
        assert!(memo.len() <= 128, "the contract's memo limit");
        let longest = payment_memo(&"n".repeat(MAX_NONCE_LEN), &secret).unwrap();
        assert_eq!(longest.len(), 128);
        assert!(memo.starts_with(&format!("{nonce}:")));
        assert!(!memo.contains(&secret), "the memo is public");
        assert_eq!(memo, payment_memo(&nonce, &secret).unwrap());

        // Another secret or another nonce is another memo.
        assert_ne!(memo, payment_memo(&nonce, &new_receipt_secret()).unwrap());
        assert_ne!(memo, payment_memo("other", &secret).unwrap());
    }

    #[test]
    fn a_receipt_secret_has_only_one_spelling() {
        let good = "ab".repeat(32);
        assert!(payment_memo("n", &good).is_ok());
        for bad in [
            String::new(),
            "ab".repeat(31),
            "ab".repeat(33),
            "AB".repeat(32),
            "zz".repeat(32),
        ] {
            assert!(payment_memo("n", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn garbage_header_is_rejected() {
        assert!(decode::<PaymentPayload>("not base64!").is_err());
    }
}
