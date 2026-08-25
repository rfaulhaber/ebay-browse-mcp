//! Single-operator OAuth 2.1 provider guarding the remote MCP endpoint.
//!
//! Claude.ai connects to a remote MCP server as an OAuth *public client*: it
//! expects an unauthenticated request to draw a 401 pointing at RFC 9728
//! protected-resource metadata, discovers the authorization server from that
//! document, registers itself via RFC 7591 dynamic client registration, and
//! then runs an authorization-code + PKCE (S256) flow. This module is the
//! minimum server half of that dance for a deployment with a single human
//! operator: "user consent" is one shared access key, and every artifact —
//! client id, authorization code, access and refresh token — is a stateless
//! HMAC-signed claims blob. Nothing is persisted (bar an in-memory replay
//! set for authorization codes), so restarts keep issued tokens working and
//! rotating the signing key revokes everything at once.
//!
//! The trade-offs of statelessness are deliberate: a rotated-away refresh
//! token remains cryptographically valid until it expires, and revocation is
//! all-or-nothing via key rotation. For one operator guarding a read-only
//! search API, that is a better deal than a token database.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::RngExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::Secret;

type HmacSha256 = Hmac<Sha256>;

/// Fixed callback for every hosted Claude surface (web, desktop, mobile).
pub const CLAUDE_CALLBACK: &str = "https://claude.ai/api/mcp/auth_callback";

/// Claude's OAuth machinery waits at most ten seconds per endpoint, and a
/// user finishing the consent form well within a code's lifetime retries
/// cleanly if they do not.
const CODE_TTL_SECS: i64 = 5 * 60;
const ACCESS_TTL_SECS: i64 = 60 * 60;
const REFRESH_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// Errors surfaced to the OAuth client. Variants map onto the RFC 6749
/// `error` codes via [`OauthError::code`]; the display text becomes
/// `error_description`, so it states the remedy.
#[derive(Debug, thiserror::Error)]
pub enum OauthError {
    #[error("client_id is malformed, foreign, or predates a signing-key rotation; re-register")]
    InvalidClient,
    #[error("redirect_uri is not registered for this client")]
    RedirectMismatch,
    #[error("{0}")]
    InvalidRequest(String),
    #[error("{0}")]
    InvalidGrant(String),
    #[error("resource does not match this server's advertised resource identifier")]
    InvalidTarget,
    #[error("unsupported grant_type; only authorization_code and refresh_token are issued")]
    UnsupportedGrantType,
    #[error("internal error: {0}")]
    Internal(String),
}

impl OauthError {
    /// The RFC 6749 / RFC 8707 `error` code for a token-endpoint response.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidClient => "invalid_client",
            Self::RedirectMismatch | Self::InvalidRequest(_) => "invalid_request",
            Self::InvalidGrant(_) => "invalid_grant",
            Self::InvalidTarget => "invalid_target",
            Self::UnsupportedGrantType => "unsupported_grant_type",
            Self::Internal(_) => "server_error",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("redirect_uris must be a non-empty array")]
    MissingRedirects,
    #[error(
        "redirect_uri {0:?} is not allowed: this server accepts the Claude callback \
         (https://claude.ai/api/mcp/auth_callback) or an http loopback URI \
         (localhost / 127.0.0.1, any port, no query or fragment)"
    )]
    DisallowedRedirect(String),
}

/// Why a presented bearer token was rejected. The display text is quoted
/// inside a `WWW-Authenticate` header, so it must stay free of `"`.
#[derive(Debug, thiserror::Error)]
pub enum BearerError {
    #[error("the access token is malformed, foreign, or was revoked by key rotation")]
    Invalid,
    #[error("the access token has expired")]
    Expired,
}

/// Everything sealed under the HMAC key. The `typ` tag gives domain
/// separation: a refresh token replayed as an access token, or a client id
/// replayed as a code, fails to open as the expected variant.
#[derive(Serialize, Deserialize)]
#[serde(tag = "typ", rename_all = "snake_case")]
enum Claims {
    Client {
        redirect_uris: Vec<String>,
        iat: i64,
    },
    Code {
        exp: i64,
        client: String,
        redirect_uri: String,
        challenge: String,
        scope: Option<String>,
        jti: String,
    },
    Access {
        exp: i64,
        aud: String,
        client: String,
        jti: String,
    },
    Refresh {
        exp: i64,
        aud: String,
        client: String,
        jti: String,
    },
}

pub struct RegisteredClient {
    pub client_id: String,
    pub redirect_uris: Vec<String>,
}

/// A successful token-endpoint response body, before serialization.
pub struct TokenGrant {
    pub access_token: String,
    pub expires_in: i64,
    pub refresh_token: String,
    pub scope: Option<String>,
}

pub struct OauthCore {
    /// Origin the OAuth endpoints live on, without a trailing slash.
    pub issuer: String,
    /// The MCP endpoint URL exactly as entered into the connecting client;
    /// RFC 9728 requires the advertised `resource` to byte-match it, and
    /// access tokens are audience-bound to it.
    pub resource: String,
    signing_key: Secret,
    access_key: Secret,
    /// Ids of already-redeemed authorization codes, for single-use
    /// enforcement. Codes live five minutes, so pruning on redemption keeps
    /// this map a handful of entries at most.
    redeemed_codes: Mutex<HashMap<String, i64>>,
}

impl OauthCore {
    #[must_use]
    pub fn new(issuer: String, resource: String, signing_key: Secret, access_key: Secret) -> Self {
        Self {
            issuer,
            resource,
            signing_key,
            access_key,
            redeemed_codes: Mutex::new(HashMap::new()),
        }
    }

    /// Constant-time check of the operator's consent key.
    #[must_use]
    pub fn access_key_matches(&self, presented: &str) -> bool {
        bool::from(
            presented
                .as_bytes()
                .ct_eq(self.access_key.expose().as_bytes()),
        )
    }

    /// RFC 7591 dynamic client registration, allowlisted to callbacks Claude
    /// clients actually use. Claude registers a fresh client on every new
    /// connection, so client ids are self-contained rather than stored.
    pub fn register_client(
        &self,
        redirect_uris: &[String],
    ) -> Result<RegisteredClient, RegistrationError> {
        if redirect_uris.is_empty() {
            return Err(RegistrationError::MissingRedirects);
        }
        let normalized = redirect_uris
            .iter()
            .map(|uri| normalize_redirect(uri))
            .collect::<Result<Vec<_>, _>>()?;
        let client_id = self
            .seal(&Claims::Client {
                redirect_uris: normalized.clone(),
                iat: now(),
            })
            .map_err(|_| RegistrationError::MissingRedirects)?;
        Ok(RegisteredClient {
            client_id,
            redirect_uris: normalized,
        })
    }

    /// Validates the (client, redirect) pair of an authorization request.
    /// Failure here means the caller must not redirect — it has no trusted
    /// destination — and should render the error instead.
    pub fn check_authorize_target(
        &self,
        client_id: &str,
        redirect_uri: &str,
    ) -> Result<(), OauthError> {
        let Some(Claims::Client { redirect_uris, .. }) = self.open(client_id) else {
            return Err(OauthError::InvalidClient);
        };
        if redirect_uris
            .iter()
            .any(|registered| redirect_matches(registered, redirect_uri))
        {
            Ok(())
        } else {
            Err(OauthError::RedirectMismatch)
        }
    }

    /// Mints the authorization code delivered through the consent redirect.
    /// The caller has already verified the access key and redirect target.
    pub fn mint_code(
        &self,
        client_id: &str,
        redirect_uri: &str,
        code_challenge: &str,
        scope: Option<&str>,
    ) -> Result<String, OauthError> {
        self.seal(&Claims::Code {
            exp: now() + CODE_TTL_SECS,
            client: client_fingerprint(client_id),
            redirect_uri: redirect_uri.to_owned(),
            challenge: code_challenge.to_owned(),
            scope: scope.map(str::to_owned),
            jti: fresh_id(),
        })
    }

    /// The `authorization_code` grant: single-use code, PKCE S256 proof,
    /// client and redirect binding.
    pub fn redeem_code(
        &self,
        code: &str,
        client_id: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenGrant, OauthError> {
        let Some(Claims::Client { .. }) = self.open(client_id) else {
            return Err(OauthError::InvalidClient);
        };
        let Some(Claims::Code {
            exp,
            client,
            redirect_uri: bound_redirect,
            challenge,
            scope,
            jti,
        }) = self.open(code)
        else {
            return Err(OauthError::InvalidGrant(
                "authorization code is malformed, foreign, or predates a signing-key rotation; \
                 restart the connection"
                    .to_owned(),
            ));
        };
        if exp < now() {
            return Err(OauthError::InvalidGrant(
                "authorization code expired; restart the connection".to_owned(),
            ));
        }
        if client != client_fingerprint(client_id) {
            return Err(OauthError::InvalidGrant(
                "authorization code was issued to a different client".to_owned(),
            ));
        }
        if bound_redirect != redirect_uri {
            return Err(OauthError::InvalidGrant(
                "redirect_uri does not match the one used at the authorization endpoint".to_owned(),
            ));
        }
        self.consume_code_id(jti)?;
        let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        if !bool::from(computed.as_bytes().ct_eq(challenge.as_bytes())) {
            return Err(OauthError::InvalidGrant(
                "PKCE verification failed: code_verifier does not match code_challenge".to_owned(),
            ));
        }
        self.mint_grant(client_id, scope)
    }

    /// The `refresh_token` grant. Rotation issues a fresh refresh token each
    /// time; see the module docs for what stateless rotation does and does
    /// not revoke.
    pub fn refresh(&self, refresh_token: &str, client_id: &str) -> Result<TokenGrant, OauthError> {
        let Some(Claims::Client { .. }) = self.open(client_id) else {
            return Err(OauthError::InvalidClient);
        };
        let Some(Claims::Refresh {
            exp, aud, client, ..
        }) = self.open(refresh_token)
        else {
            return Err(OauthError::InvalidGrant(
                "refresh token is malformed, foreign, or was revoked by key rotation; \
                 reconnect the server"
                    .to_owned(),
            ));
        };
        if exp < now() {
            return Err(OauthError::InvalidGrant(
                "refresh token expired; reconnect the server".to_owned(),
            ));
        }
        if aud != self.resource {
            return Err(OauthError::InvalidGrant(
                "refresh token was issued for a different resource".to_owned(),
            ));
        }
        if client != client_fingerprint(client_id) {
            return Err(OauthError::InvalidGrant(
                "refresh token was issued to a different client".to_owned(),
            ));
        }
        self.mint_grant(client_id, None)
    }

    /// Resource-server side: is this bearer token one of ours, unexpired,
    /// and audience-bound to this MCP endpoint?
    pub fn validate_bearer(&self, token: &str) -> Result<(), BearerError> {
        let Some(Claims::Access { exp, aud, .. }) = self.open(token) else {
            return Err(BearerError::Invalid);
        };
        if aud != self.resource {
            return Err(BearerError::Invalid);
        }
        if exp < now() {
            return Err(BearerError::Expired);
        }
        Ok(())
    }

    fn mint_grant(&self, client_id: &str, scope: Option<String>) -> Result<TokenGrant, OauthError> {
        let client = client_fingerprint(client_id);
        let access_token = self.seal(&Claims::Access {
            exp: now() + ACCESS_TTL_SECS,
            aud: self.resource.clone(),
            client: client.clone(),
            jti: fresh_id(),
        })?;
        let refresh_token = self.seal(&Claims::Refresh {
            exp: now() + REFRESH_TTL_SECS,
            aud: self.resource.clone(),
            client,
            jti: fresh_id(),
        })?;
        Ok(TokenGrant {
            access_token,
            expires_in: ACCESS_TTL_SECS,
            refresh_token,
            scope,
        })
    }

    fn consume_code_id(&self, jti: String) -> Result<(), OauthError> {
        let mut redeemed = self
            .redeemed_codes
            .lock()
            .map_err(|_| OauthError::Internal("code replay set poisoned".to_owned()))?;
        let cutoff = now();
        redeemed.retain(|_, exp| *exp >= cutoff);
        if redeemed.insert(jti, cutoff + CODE_TTL_SECS).is_some() {
            return Err(OauthError::InvalidGrant(
                "authorization code already redeemed".to_owned(),
            ));
        }
        Ok(())
    }

    fn seal(&self, claims: &Claims) -> Result<String, OauthError> {
        let payload = serde_json::to_vec(claims)
            .map_err(|err| OauthError::Internal(format!("claims serialization: {err}")))?;
        let tag = self.mac(&payload)?;
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(tag)
        ))
    }

    /// Returns `None` for anything not sealed by this server's current key:
    /// bad framing, bad MAC, or unparseable claims.
    fn open(&self, blob: &str) -> Option<Claims> {
        let (payload_b64, tag_b64) = blob.split_once('.')?;
        let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
        let tag = URL_SAFE_NO_PAD.decode(tag_b64).ok()?;
        let mut mac = HmacSha256::new_from_slice(self.signing_key.expose().as_bytes()).ok()?;
        mac.update(&payload);
        mac.verify_slice(&tag).ok()?;
        serde_json::from_slice(&payload).ok()
    }

    fn mac(&self, payload: &[u8]) -> Result<Vec<u8>, OauthError> {
        let mut mac = HmacSha256::new_from_slice(self.signing_key.expose().as_bytes())
            .map_err(|err| OauthError::Internal(format!("HMAC key: {err}")))?;
        mac.update(payload);
        Ok(mac.finalize().into_bytes().to_vec())
    }
}

/// A registered redirect must be the Claude callback verbatim or an http
/// loopback URI, which is stored with its port stripped: RFC 8252 §7.3
/// requires port-agnostic matching for native clients (Claude Code, MCP
/// Inspector) that bind an ephemeral port per flow.
fn normalize_redirect(uri: &str) -> Result<String, RegistrationError> {
    if uri == CLAUDE_CALLBACK {
        return Ok(uri.to_owned());
    }
    let disallowed = || RegistrationError::DisallowedRedirect(uri.to_owned());
    let parsed = url::Url::parse(uri).map_err(|_| disallowed())?;
    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1"));
    if parsed.scheme() != "http"
        || !loopback
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(disallowed());
    }
    let host = parsed.host_str().unwrap_or_default();
    Ok(format!("http://{host}{}", parsed.path()))
}

/// Exact match for https callbacks; host + path match with the port ignored
/// for normalized loopback entries.
fn redirect_matches(registered: &str, presented: &str) -> bool {
    if registered == presented {
        return true;
    }
    if !registered.starts_with("http://") {
        return false;
    }
    let (Ok(registered), Ok(presented)) = (url::Url::parse(registered), url::Url::parse(presented))
    else {
        return false;
    };
    presented.scheme() == "http"
        && presented.host_str() == registered.host_str()
        && presented.path() == registered.path()
        && presented.query().is_none()
        && presented.fragment().is_none()
}

/// Short binding of tokens to the client they were issued to. A hash keeps
/// codes and tokens compact instead of embedding the (already long) sealed
/// client id.
fn client_fingerprint(client_id: &str) -> String {
    let digest = Sha256::digest(client_id.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..12])
}

fn fresh_id() -> String {
    let bytes: [u8; 16] = rand::rng().random();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions are clearer unwrapped")]

    use super::*;

    fn core() -> OauthCore {
        OauthCore::new(
            "https://mcp.example.test".to_owned(),
            "https://mcp.example.test/mcp".to_owned(),
            Secret::new("0123456789abcdef0123456789abcdef"),
            Secret::new("correct-horse-battery-staple"),
        )
    }

    fn register(core: &OauthCore) -> String {
        core.register_client(&[CLAUDE_CALLBACK.to_owned()])
            .unwrap()
            .client_id
    }

    fn challenge_for(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    #[test]
    fn full_grant_roundtrip() {
        let core = core();
        let client_id = register(&core);
        let verifier = "a".repeat(43);
        let code = core
            .mint_code(&client_id, CLAUDE_CALLBACK, &challenge_for(&verifier), None)
            .unwrap();
        let grant = core
            .redeem_code(&code, &client_id, CLAUDE_CALLBACK, &verifier)
            .unwrap();
        core.validate_bearer(&grant.access_token).unwrap();
        let rotated = core.refresh(&grant.refresh_token, &client_id).unwrap();
        core.validate_bearer(&rotated.access_token).unwrap();
        assert_ne!(grant.access_token, rotated.access_token);
        assert_ne!(grant.refresh_token, rotated.refresh_token);
    }

    #[test]
    fn code_is_single_use() {
        let core = core();
        let client_id = register(&core);
        let verifier = "b".repeat(43);
        let code = core
            .mint_code(&client_id, CLAUDE_CALLBACK, &challenge_for(&verifier), None)
            .unwrap();
        core.redeem_code(&code, &client_id, CLAUDE_CALLBACK, &verifier)
            .unwrap();
        let replay = core.redeem_code(&code, &client_id, CLAUDE_CALLBACK, &verifier);
        assert!(matches!(replay, Err(OauthError::InvalidGrant(_))));
    }

    #[test]
    fn wrong_pkce_verifier_is_rejected() {
        let core = core();
        let client_id = register(&core);
        let code = core
            .mint_code(&client_id, CLAUDE_CALLBACK, &challenge_for("right"), None)
            .unwrap();
        let result = core.redeem_code(&code, &client_id, CLAUDE_CALLBACK, "wrong");
        assert!(matches!(result, Err(OauthError::InvalidGrant(_))));
    }

    #[test]
    fn tokens_do_not_cross_purposes() {
        let core = core();
        let client_id = register(&core);
        let verifier = "c".repeat(43);
        let code = core
            .mint_code(&client_id, CLAUDE_CALLBACK, &challenge_for(&verifier), None)
            .unwrap();
        let grant = core
            .redeem_code(&code, &client_id, CLAUDE_CALLBACK, &verifier)
            .unwrap();
        // Refresh token as bearer: rejected.
        assert!(core.validate_bearer(&grant.refresh_token).is_err());
        // Access token as refresh token: rejected.
        assert!(core.refresh(&grant.access_token, &client_id).is_err());
        // Client id as bearer: rejected.
        assert!(core.validate_bearer(&client_id).is_err());
    }

    #[test]
    fn tampered_token_is_rejected() {
        let core = core();
        let client_id = register(&core);
        let verifier = "d".repeat(43);
        let code = core
            .mint_code(&client_id, CLAUDE_CALLBACK, &challenge_for(&verifier), None)
            .unwrap();
        let grant = core
            .redeem_code(&code, &client_id, CLAUDE_CALLBACK, &verifier)
            .unwrap();
        let mut forged = grant.access_token.clone();
        forged.replace_range(0..1, if forged.starts_with('A') { "B" } else { "A" });
        assert!(core.validate_bearer(&forged).is_err());
    }

    #[test]
    fn foreign_key_tokens_are_rejected() {
        let ours = core();
        let theirs = OauthCore::new(
            ours.issuer.clone(),
            ours.resource.clone(),
            Secret::new("ffffffffffffffffffffffffffffffff"),
            Secret::new("correct-horse-battery-staple"),
        );
        let client_id = register(&theirs);
        let verifier = "e".repeat(43);
        let code = theirs
            .mint_code(&client_id, CLAUDE_CALLBACK, &challenge_for(&verifier), None)
            .unwrap();
        let grant = theirs
            .redeem_code(&code, &client_id, CLAUDE_CALLBACK, &verifier)
            .unwrap();
        assert!(ours.validate_bearer(&grant.access_token).is_err());
    }

    #[test]
    fn expired_access_token_is_rejected() {
        let core = core();
        let expired = core
            .seal(&Claims::Access {
                exp: now() - 1,
                aud: core.resource.clone(),
                client: "x".to_owned(),
                jti: "y".to_owned(),
            })
            .unwrap();
        assert!(matches!(
            core.validate_bearer(&expired),
            Err(BearerError::Expired)
        ));
    }

    #[test]
    fn wrong_audience_is_rejected() {
        let core = core();
        let foreign_aud = core
            .seal(&Claims::Access {
                exp: now() + 60,
                aud: "https://other.example.test/mcp".to_owned(),
                client: "x".to_owned(),
                jti: "y".to_owned(),
            })
            .unwrap();
        assert!(matches!(
            core.validate_bearer(&foreign_aud),
            Err(BearerError::Invalid)
        ));
    }

    #[test]
    fn registration_allowlist() {
        let core = core();
        assert!(core.register_client(&[]).is_err());
        assert!(
            core.register_client(&["https://evil.example/callback".to_owned()])
                .is_err()
        );
        assert!(
            core.register_client(&["http://localhost/callback?x=1".to_owned()])
                .is_err()
        );
        let client = core
            .register_client(&[
                CLAUDE_CALLBACK.to_owned(),
                "http://localhost:3118/callback".to_owned(),
                "http://127.0.0.1:41999/cb".to_owned(),
            ])
            .unwrap();
        // Loopback entries are stored port-stripped.
        assert!(
            client
                .redirect_uris
                .contains(&"http://localhost/callback".to_owned())
        );
        assert!(
            client
                .redirect_uris
                .contains(&"http://127.0.0.1/cb".to_owned())
        );
    }

    #[test]
    fn loopback_redirect_matches_any_port() {
        let core = core();
        let client = core
            .register_client(&["http://localhost:3118/callback".to_owned()])
            .unwrap();
        assert!(
            core.check_authorize_target(&client.client_id, "http://localhost:53211/callback")
                .is_ok()
        );
        assert!(
            core.check_authorize_target(&client.client_id, "http://localhost:53211/other")
                .is_err()
        );
        assert!(
            core.check_authorize_target(&client.client_id, CLAUDE_CALLBACK)
                .is_err()
        );
    }

    #[test]
    fn code_bound_to_client_and_redirect() {
        let core = core();
        let client_a = register(&core);
        let client_b = core
            .register_client(&[
                CLAUDE_CALLBACK.to_owned(),
                "http://localhost/callback".to_owned(),
            ])
            .unwrap()
            .client_id;
        let verifier = "f".repeat(43);
        let code = core
            .mint_code(&client_a, CLAUDE_CALLBACK, &challenge_for(&verifier), None)
            .unwrap();
        assert!(
            core.redeem_code(&code, &client_b, CLAUDE_CALLBACK, &verifier)
                .is_err()
        );
        assert!(
            core.redeem_code(&code, &client_a, "http://localhost/callback", &verifier)
                .is_err()
        );
    }

    #[test]
    fn access_key_comparison() {
        let core = core();
        assert!(core.access_key_matches("correct-horse-battery-staple"));
        assert!(!core.access_key_matches("correct-horse-battery-stapl"));
        assert!(!core.access_key_matches(""));
    }
}
