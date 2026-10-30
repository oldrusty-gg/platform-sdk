//! Platform access tokens: short-lived, Ed25519-signed statements by the
//! account service ("user 42, named Yugi, supporter, for the roses server,
//! until 12:05"). A game server verifies them offline with the service's
//! public key(s): no database, no call to the account service, and a game
//! server that leaks its configuration leaks no secret.
//!
//! Format: `pt1.<base64url(claims JSON)>.<base64url(signature)>`, the
//! signature covering `pt1.<claims>` (the ASCII text before the last dot).

use crate::b64;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "pt1";
/// Clock difference tolerated between the account service and a game server.
pub const LEEWAY_SECONDS: i64 = 60;
/// The longest lifetime a verifier accepts (a token is re-issued from the
/// session well before).
pub const MAX_LIFETIME_SECONDS: i64 = 24 * 3600;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The platform user id.
    pub sub: i64,
    /// Display name at issue time.
    pub name: String,
    /// The game server this token is for ([`crate::manifest::OnlineSpec::audience`]).
    pub aud: String,
    pub iat: i64,
    pub exp: i64,
    /// "admin", "moderator", ...
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// Supporter perks (cosmetic, platform-level only; see the plan).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub supporter: u32,
    /// The signing key's id (for rotation).
    pub kid: String,
    /// The email address is verified.
    #[serde(default)]
    pub email_verified: bool,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    UnknownKey,
    BadSignature,
    Expired,
    NotYetValid,
    WrongAudience,
    TooLong,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The account service's signing key.
pub struct Signer {
    kid: String,
    pair: Ed25519KeyPair,
}

impl Signer {
    /// A new random key (PKCS#8 bytes returned for storage: keep them secret).
    pub fn generate(kid: &str) -> Result<(Self, Vec<u8>), String> {
        let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| "key generation failed".to_string())?;
        let bytes = doc.as_ref().to_vec();
        Ok((Self::from_pkcs8(kid, &bytes)?, bytes))
    }

    pub fn from_pkcs8(kid: &str, pkcs8: &[u8]) -> Result<Self, String> {
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| "not an Ed25519 PKCS#8 key".to_string())?;
        Ok(Self { kid: kid.into(), pair })
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The public key to give game servers.
    pub fn public_key(&self) -> PublicKey {
        PublicKey { kid: self.kid.clone(), key: b64::encode(self.pair.public_key().as_ref()) }
    }

    /// Sign `claims` (its `kid` is set to this key's).
    pub fn sign(&self, mut claims: Claims) -> String {
        claims.kid = self.kid.clone();
        let body = format!("{PREFIX}.{}", b64::encode(&serde_json::to_vec(&claims).expect("claims serialise")));
        let sig = self.pair.sign(body.as_bytes());
        format!("{body}.{}", b64::encode(sig.as_ref()))
    }
}

/// One public key a verifier trusts (a game server's config lists the
/// current and the previous one during a rotation).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKey {
    pub kid: String,
    /// base64url of the 32-byte Ed25519 public key.
    pub key: String,
}

/// Check `token` for `audience` at `now` (seconds since 1970) against the
/// trusted `keys`.
pub fn verify(token: &str, keys: &[PublicKey], audience: &str, now: i64) -> Result<Claims, TokenError> {
    if token.len() > 4096 {
        return Err(TokenError::TooLong);
    }
    let (body, sig) = token.rsplit_once('.').ok_or(TokenError::Malformed)?;
    let claims_b64 = body.strip_prefix(PREFIX).and_then(|r| r.strip_prefix('.')).ok_or(TokenError::Malformed)?;
    let claims: Claims = serde_json::from_slice(&b64::decode(claims_b64).ok_or(TokenError::Malformed)?).map_err(|_| TokenError::Malformed)?;
    let sig = b64::decode(sig).ok_or(TokenError::Malformed)?;
    let key = keys.iter().find(|k| k.kid == claims.kid).ok_or(TokenError::UnknownKey)?;
    let raw = b64::decode(&key.key).ok_or(TokenError::UnknownKey)?;
    UnparsedPublicKey::new(&ED25519, raw).verify(body.as_bytes(), &sig).map_err(|_| TokenError::BadSignature)?;
    if claims.aud != audience && claims.aud != "*" {
        return Err(TokenError::WrongAudience);
    }
    if claims.iat > now + LEEWAY_SECONDS {
        return Err(TokenError::NotYetValid);
    }
    if claims.exp + LEEWAY_SECONDS < now || claims.exp - claims.iat > MAX_LIFETIME_SECONDS {
        return Err(TokenError::Expired);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(now: i64) -> Claims {
        Claims {
            sub: 42,
            name: "Yugi".into(),
            aud: "roses".into(),
            iat: now,
            exp: now + 900,
            roles: vec![],
            supporter: 1,
            kid: String::new(),
            email_verified: true,
        }
    }

    #[test]
    fn a_signed_token_verifies_only_as_issued() {
        let (signer, pkcs8) = Signer::generate("k1").unwrap();
        let keys = [signer.public_key()];
        let now = 1_800_000_000;
        let token = signer.sign(claims(now));
        assert!(token.starts_with("pt1."));
        let c = verify(&token, &keys, "roses", now + 10).unwrap();
        assert_eq!((c.sub, c.kid.as_str(), c.supporter), (42, "k1", 1));
        // The same key loads back from its stored bytes.
        assert_eq!(Signer::from_pkcs8("k1", &pkcs8).unwrap().public_key(), keys[0]);
        assert_eq!(verify(&token, &keys, "other-game", now), Err(TokenError::WrongAudience));
        assert_eq!(verify(&token, &keys, "roses", now + 2000), Err(TokenError::Expired));
        assert_eq!(verify(&token, &keys, "roses", now - 1000), Err(TokenError::NotYetValid));
        // A changed claim breaks the signature.
        let (body, sig) = token.rsplit_once('.').unwrap();
        let forged_claims = Claims { sub: 1, kid: "k1".into(), ..claims(now) };
        let forged = format!("pt1.{}.{sig}", b64::encode(&serde_json::to_vec(&forged_claims).unwrap()));
        assert_eq!(verify(&forged, &keys, "roses", now), Err(TokenError::BadSignature));
        let _ = body;
        // Another service's key is not trusted.
        let (other, _) = Signer::generate("k1").unwrap();
        assert_eq!(verify(&other.sign(claims(now)), &keys, "roses", now), Err(TokenError::BadSignature));
        assert_eq!(verify("pt1.x", &keys, "roses", now), Err(TokenError::Malformed));
        let (k2, _) = Signer::generate("k2").unwrap();
        assert_eq!(verify(&k2.sign(claims(now)), &keys, "roses", now), Err(TokenError::UnknownKey));
    }
}
