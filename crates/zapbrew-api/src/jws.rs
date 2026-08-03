//! Homebrew JSON API JWS envelope verification (RFC 7515 + RFC 7797 unencoded payload).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::pkcs8::DecodePublicKey;
use rsa::sha2::{Digest, Sha512};
use rsa::{Pss, RsaPublicKey};
use serde::Deserialize;
use serde_json::Value;

use crate::error::ApiError;

const HOMEBREW_PUBLIC_KEY_PEM: &str = include_str!("homebrew-1.pem");

/// Object-safe JWS envelope verifier.
pub(crate) trait JwsVerifier: Send + Sync {
    /// Verify a JWS JSON envelope and return the raw `payload` string bytes unchanged.
    fn verify(&self, envelope: &[u8]) -> Result<Vec<u8>, ApiError>;
}

/// Verifier that trusts an RSA public key (production: embedded Homebrew key).
pub(crate) struct HomebrewVerifier {
    public_key: RsaPublicKey,
}

impl HomebrewVerifier {
    /// Production constructor: embed the exact Homebrew `homebrew-1` public key.
    pub(crate) fn new() -> Result<Self, ApiError> {
        Self::from_pem(HOMEBREW_PUBLIC_KEY_PEM)
    }

    /// Test/internal constructor: inject an alternate public key PEM.
    ///
    /// Production code must call [`Self::new`]; there is no verification bypass.
    pub(crate) fn from_pem(pem: &str) -> Result<Self, ApiError> {
        let public_key = RsaPublicKey::from_public_key_pem(pem).map_err(|err| {
            ApiError::invalid("jws public key", format!("failed to parse PEM: {err}"))
        })?;
        Ok(Self { public_key })
    }
}

impl JwsVerifier for HomebrewVerifier {
    fn verify(&self, envelope: &[u8]) -> Result<Vec<u8>, ApiError> {
        let parsed: Envelope =
            serde_json::from_slice(envelope).map_err(|source| ApiError::Json {
                context: "jws envelope".to_owned(),
                source,
            })?;

        let payload = match parsed.payload {
            Value::String(s) => s,
            _ => {
                return Err(ApiError::invalid(
                    "jws envelope",
                    "payload must be a JSON string",
                ));
            }
        };

        let signatures = parsed
            .signatures
            .ok_or_else(|| ApiError::invalid("jws envelope", "missing signatures array"))?;

        if signatures.is_empty() {
            return Err(ApiError::Signature {
                reason: "no valid signature".to_owned(),
            });
        }

        let mut saw_supported = false;
        let mut saw_unsupported = false;
        let mut saw_malformed = false;

        for signature in &signatures {
            match self.try_signature(&payload, signature) {
                SigOutcome::Valid => return Ok(payload.into_bytes()),
                SigOutcome::Unsupported => saw_unsupported = true,
                SigOutcome::Malformed => saw_malformed = true,
                SigOutcome::Mismatch => saw_supported = true,
            }
        }

        if saw_supported {
            return Err(ApiError::Signature {
                reason: "no valid signature".to_owned(),
            });
        }
        if saw_unsupported {
            return Err(ApiError::invalid(
                "jws protected header",
                "unsupported algorithm",
            ));
        }
        if saw_malformed {
            return Err(ApiError::invalid(
                "jws signature",
                "malformed signature entry",
            ));
        }

        Err(ApiError::Signature {
            reason: "no valid signature".to_owned(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    payload: Value,
    signatures: Option<Vec<SignatureEntry>>,
}

#[derive(Debug, Deserialize)]
struct SignatureEntry {
    protected: Option<Value>,
    signature: Option<Value>,
    /// Present in Homebrew envelopes (`kid`); unused for verification selection.
    #[serde(default)]
    #[allow(dead_code)]
    header: Option<Value>,
}

#[derive(Debug)]
enum SigOutcome {
    Valid,
    Unsupported,
    Malformed,
    Mismatch,
}

impl HomebrewVerifier {
    fn try_signature(&self, payload: &str, entry: &SignatureEntry) -> SigOutcome {
        let protected_b64 = match entry.protected.as_ref() {
            Some(Value::String(s)) => s.as_str(),
            _ => return SigOutcome::Malformed,
        };
        let signature_b64 = match entry.signature.as_ref() {
            Some(Value::String(s)) => s.as_str(),
            _ => return SigOutcome::Malformed,
        };

        let protected_bytes = match URL_SAFE_NO_PAD.decode(protected_b64.as_bytes()) {
            Ok(bytes) => bytes,
            Err(_) => return SigOutcome::Malformed,
        };
        let header: Value = match serde_json::from_slice(&protected_bytes) {
            Ok(v) => v,
            Err(_) => return SigOutcome::Malformed,
        };

        match require_ps512_unencoded(&header) {
            Ok(()) => {}
            Err(HeaderReject::Unsupported) => return SigOutcome::Unsupported,
            Err(HeaderReject::Malformed) => return SigOutcome::Malformed,
        }

        let signature_bytes = match URL_SAFE_NO_PAD.decode(signature_b64.as_bytes()) {
            Ok(bytes) => bytes,
            // Header was a supported PS512/b64:false candidate; treat decode failure
            // as a failed verification attempt (not "unsupported algorithm").
            Err(_) => return SigOutcome::Mismatch,
        };

        // RFC 7797: signing input is protected_b64 || '.' || raw payload bytes.
        // Do not re-encode/normalize protected or payload.
        let mut message = Vec::with_capacity(protected_b64.len() + 1 + payload.len());
        message.extend_from_slice(protected_b64.as_bytes());
        message.push(b'.');
        message.extend_from_slice(payload.as_bytes());

        let hashed = Sha512::digest(&message);
        match self
            .public_key
            .verify(Pss::new_with_salt::<Sha512>(64), &hashed, &signature_bytes)
        {
            Ok(()) => SigOutcome::Valid,
            Err(_) => SigOutcome::Mismatch,
        }
    }
}

#[derive(Debug)]
enum HeaderReject {
    Unsupported,
    Malformed,
}

fn require_ps512_unencoded(header: &Value) -> Result<(), HeaderReject> {
    let obj = header.as_object().ok_or(HeaderReject::Malformed)?;
    let alg = obj.get("alg").ok_or(HeaderReject::Malformed)?;
    let b64 = obj.get("b64").ok_or(HeaderReject::Malformed)?;

    let alg_ok = matches!(alg, Value::String(s) if s == "PS512");
    // NOTE: nil/missing b64 means true in JOSE; we require explicit false.
    let b64_ok = matches!(b64, Value::Bool(false));

    if alg_ok && b64_ok {
        Ok(())
    } else if obj.contains_key("alg") && obj.contains_key("b64") {
        Err(HeaderReject::Unsupported)
    } else {
        Err(HeaderReject::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic test public key (OpenSSL RSA-2048); paired with FIXED_* fixtures.
    const TEST_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAyAiY7/10W7Yji+xoGArm\n\
ZSo8MImAreRFBlGeMxKUplwAJ3L4eBj6uPLo29dEt7wrmGqzlorgAKtvAZZnE7U+\n\
u3VWpSr3fzkH4s9Eb3JjvCcR7jWEpr0ZnMX2c59OE+XhUDUDxLBQPFTVAxYT1xTD\n\
M0Z4vP6aRAN7Z5aoITDEfRPL8CftYlqNopDZGneIzc/FL/5LMzteAysXjfKLpGFJ\n\
PDOT0wTMkCJCLBCQ8M8Fcq3C8yytCLSBdfcmH+Ew/rzryl566QVxpg3VVQG6mvgX\n\
90TFpBVf0YQjzwb8Z/HPs/OoaH9bFADFELLa2FJg4J5iQyWBelmM294XkzPCLnZF\n\
4QIDAQAB\n\
-----END PUBLIC KEY-----\n";

    const PROTECTED_B64: &str = "eyJhbGciOiJQUzUxMiIsImI2NCI6ZmFsc2V9";
    const PAYLOAD: &str = "{\"foo\":\"bar\"}";
    const FIXED_SIGNATURE_B64: &str = "ax5aAEY53JGDaW762PJcGdlFZiCr2hDm0rlWWhbEyTE-48xegO2i-iAH1wi7hdsj8La0YDklNW-XebMEqrfqwdz_eNxuCNoqHQjRxSZwy68IJRVrDQSam3ZrE6_KoNVDcD63ItxECnnsaviEl9lkY_a3wR8rrRl_1wBJ-g4pTQQB-R-uJkIxQ3jX01A8JkPygPYYkHZTB-MK4aUT-7a5J7ngnTJuNe7pNxKtuD4ayrZdLd4XX637O2Qi0ZwAIwAHE5kY7SaZrJUcKSC5mN9_DMMg0fMujeR6Jt0i6Xd2iwXXgLZYKgOI9_dIK9Mz4iRZIB46PudIJRToLRbF13haLA";

    fn test_verifier() -> HomebrewVerifier {
        match HomebrewVerifier::from_pem(TEST_PUBLIC_KEY_PEM) {
            Ok(v) => v,
            Err(err) => panic!("test key must parse: {err}"),
        }
    }

    fn envelope(payload: &str, signatures: Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "payload": payload,
            "signatures": signatures,
        }))
        .expect("envelope json")
    }

    fn valid_sig_entry() -> Value {
        serde_json::json!({
            "header": { "kid": "homebrew-1" },
            "protected": PROTECTED_B64,
            "signature": FIXED_SIGNATURE_B64,
        })
    }

    #[test]
    fn embedded_homebrew_key_loads() {
        match HomebrewVerifier::new() {
            Ok(_) => {}
            Err(err) => panic!("embedded Homebrew key must load: {err}"),
        }
    }

    #[test]
    fn verifies_fixed_valid_signature() {
        let verifier = test_verifier();
        let bytes = envelope(PAYLOAD, serde_json::json!([valid_sig_entry()]));
        let payload = match verifier.verify(&bytes) {
            Ok(p) => p,
            Err(err) => panic!("expected valid signature, got {err}"),
        };
        assert_eq!(payload, PAYLOAD.as_bytes());
    }

    #[test]
    fn any_valid_signature_succeeds_among_multiple() {
        let verifier = test_verifier();
        let unsupported = serde_json::json!({
            "header": { "kid": "other" },
            "protected": "eyJhbGciOiJSUzUxMiIsImI2NCI6ZmFsc2V9",
            "signature": FIXED_SIGNATURE_B64,
        });
        let bytes = envelope(PAYLOAD, serde_json::json!([unsupported, valid_sig_entry()]));
        let payload = match verifier.verify(&bytes) {
            Ok(p) => p,
            Err(err) => panic!("expected multi-sig success, got {err}"),
        };
        assert_eq!(payload, PAYLOAD.as_bytes());
    }

    #[test]
    fn rejects_payload_mismatch_without_bypass() {
        let verifier = test_verifier();
        let bytes = envelope("{\"foo\":\"evil\"}", serde_json::json!([valid_sig_entry()]));
        match verifier.verify(&bytes) {
            Err(ApiError::Signature { reason }) => {
                assert!(reason.contains("no valid signature"), "{reason}");
            }
            other => panic!("expected Signature error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_algorithm() {
        let verifier = test_verifier();
        let bad = serde_json::json!([{
            "header": { "kid": "homebrew-1" },
            "protected": "eyJhbGciOiJSUzUxMiIsImI2NCI6ZmFsc2V9",
            "signature": FIXED_SIGNATURE_B64,
        }]);
        let bytes = envelope(PAYLOAD, bad);
        match verifier.verify(&bytes) {
            Err(ApiError::InvalidData { context, reason }) => {
                assert!(context.contains("jws"), "{context}");
                assert!(reason.contains("unsupported"), "{reason}");
            }
            other => panic!("expected InvalidData unsupported, got {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed_envelope_json() {
        let verifier = test_verifier();
        match verifier.verify(b"{not-json") {
            Err(ApiError::Json { context, .. }) => {
                assert_eq!(context, "jws envelope");
            }
            other => panic!("expected Json error, got {other:?}"),
        }
    }

    #[test]
    fn does_not_normalize_payload_bytes() {
        // Fixed signature covers PAYLOAD exactly; a whitespace-normalized form would fail.
        let verifier = test_verifier();
        let bytes = envelope(PAYLOAD, serde_json::json!([valid_sig_entry()]));
        let got = match verifier.verify(&bytes) {
            Ok(p) => p,
            Err(err) => panic!("{err}"),
        };
        assert_eq!(got.as_slice(), PAYLOAD.as_bytes());
        assert_ne!(got.as_slice(), b"{\"foo\": \"bar\"}");
    }

    #[test]
    fn jws_verifier_is_object_safe() {
        let verifier = test_verifier();
        let boxed: Box<dyn JwsVerifier> = Box::new(verifier);
        let bytes = envelope(PAYLOAD, serde_json::json!([valid_sig_entry()]));
        match boxed.verify(&bytes) {
            Ok(payload) => assert_eq!(payload, PAYLOAD.as_bytes()),
            Err(err) => panic!("{err}"),
        }
    }

    #[test]
    fn production_new_uses_homebrew_pem_not_test_key() {
        // Embedded key must differ from the test fixture key (no silent swap / bypass).
        assert_ne!(HOMEBREW_PUBLIC_KEY_PEM, TEST_PUBLIC_KEY_PEM);
        assert!(HOMEBREW_PUBLIC_KEY_PEM.contains("MIICIjANBgkqhkiG9w0BAQEFAAOCAg8AMIICCgKCAgEA"));
    }
}
