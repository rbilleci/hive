use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

/// Verifies and mints the same `sf_session=<uuid>.<lowercase-hex HMAC-SHA-256>` cookie
/// `SignedCookiePrincipalVerifier.java` does.
#[derive(Clone)]
pub struct SessionVerifier {
    signing_key: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("An authenticated session is required.")]
    Missing,
    #[error("The session signature is invalid.")]
    BadSignature,
    #[error("The session claim is invalid.")]
    BadClaim,
}

impl SessionVerifier {
    pub fn new(signing_key: impl Into<String>) -> Self {
        let signing_key = signing_key.into();
        assert!(
            !signing_key.trim().is_empty(),
            "HIVE_IDENTITY_SIGNING_KEY is required."
        );
        Self {
            signing_key: signing_key.into_bytes(),
        }
    }

    /// Extracts `sf_session` from a raw `Cookie` header value and verifies it.
    pub fn verify_cookie_header(&self, cookie_header: Option<&str>) -> Result<Uuid, SessionError> {
        let cookie = cookie_header
            .and_then(|header| find_cookie(header, "sf_session"))
            .ok_or(SessionError::Missing)?;
        self.verify(cookie)
    }

    pub fn verify(&self, cookie: &str) -> Result<Uuid, SessionError> {
        let separator = cookie.rfind('.').ok_or(SessionError::BadSignature)?;
        if separator == 0 || separator == cookie.len() - 1 {
            return Err(SessionError::BadSignature);
        }
        let (claim, rest) = cookie.split_at(separator);
        let signature = &rest[1..];

        let expected = hex::encode(self.sign(claim));
        if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
            return Err(SessionError::BadSignature);
        }

        Uuid::parse_str(claim).map_err(|_| SessionError::BadClaim)
    }

    pub fn issue(&self, principal: Uuid) -> String {
        let claim = principal.to_string();
        let signature = hex::encode(self.sign(&claim));
        format!("{claim}.{signature}")
    }

    fn sign(&self, claim: &str) -> impl AsRef<[u8]> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.signing_key).expect("HMAC accepts any key length");
        mac.update(claim.as_bytes());
        mac.finalize().into_bytes()
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn find_cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').map(str::trim).find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_a_cookie_its_own_verifier_accepts() {
        let verifier = SessionVerifier::new("test-signing-key");
        let principal = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let cookie = verifier.issue(principal);
        assert_eq!(verifier.verify(&cookie).unwrap(), principal);
    }

    #[test]
    fn rejects_a_tampered_signature() {
        let verifier = SessionVerifier::new("test-signing-key");
        let principal = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let mut cookie = verifier.issue(principal);
        cookie.push('0');
        assert!(matches!(
            verifier.verify(&cookie),
            Err(SessionError::BadSignature)
        ));
    }

    #[test]
    fn rejects_a_cookie_signed_by_a_different_key() {
        let issuer = SessionVerifier::new("key-a");
        let verifier = SessionVerifier::new("key-b");
        let principal = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let cookie = issuer.issue(principal);
        assert!(matches!(
            verifier.verify(&cookie),
            Err(SessionError::BadSignature)
        ));
    }

    #[test]
    fn rejects_a_non_uuid_claim_with_a_valid_signature() {
        let verifier = SessionVerifier::new("test-signing-key");
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-signing-key").unwrap();
        mac.update(b"not-a-uuid");
        let signature = hex::encode(mac.finalize().into_bytes());
        let cookie = format!("not-a-uuid.{signature}");
        assert!(matches!(
            verifier.verify(&cookie),
            Err(SessionError::BadClaim)
        ));
    }

    #[test]
    fn rejects_a_cookie_with_no_separator() {
        let verifier = SessionVerifier::new("test-signing-key");
        assert!(matches!(
            verifier.verify("nodothere"),
            Err(SessionError::BadSignature)
        ));
    }

    #[test]
    fn extracts_the_named_cookie_from_a_header_with_others_present() {
        let verifier = SessionVerifier::new("test-signing-key");
        let principal = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let cookie = verifier.issue(principal);
        let header = format!("other=1; sf_session={cookie}; another=2");
        assert_eq!(
            verifier.verify_cookie_header(Some(&header)).unwrap(),
            principal
        );
    }

    #[test]
    fn missing_cookie_header_is_missing_session() {
        let verifier = SessionVerifier::new("test-signing-key");
        assert!(matches!(
            verifier.verify_cookie_header(None),
            Err(SessionError::Missing)
        ));
    }
}
