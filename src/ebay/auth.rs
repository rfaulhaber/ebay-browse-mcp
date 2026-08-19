//! Application access tokens via the OAuth 2.0 client-credentials grant.
//!
//! The Browse API's search and item-retrieval methods accept an *application*
//! token, which needs no end-user consent — the server can mint one from its
//! own keyset and refresh it forever. (The cart methods, which this server does
//! not expose, would require a user token obtained through the authorization
//! code grant.)

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::config::Secret;

/// The scope granting read access to public item data. eBay uses the
/// production hostname in the scope identifier even for sandbox tokens.
const API_SCOPE: &str = "https://api.ebay.com/oauth/api_scope";

/// Renew this long before the token actually lapses, so a request issued just
/// after a check cannot arrive at eBay with an expired credential.
const EXPIRY_MARGIN: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("could not reach eBay's OAuth endpoint: {0}")]
    Transport(#[source] reqwest::Error),
    #[error(
        "eBay rejected the application credentials (HTTP {status}): {error}{}",
        description.as_deref().map(|d| format!(" - {d}")).unwrap_or_default()
    )]
    Rejected {
        status: u16,
        error: String,
        description: Option<String>,
    },
    #[error("eBay returned an unreadable OAuth response (HTTP {status}): {body}")]
    Malformed { status: u16, body: String },
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Lifetime in seconds; eBay currently issues application tokens for 7200.
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    error_description: Option<String>,
}

#[derive(Debug)]
struct CachedToken {
    /// `Arc<str>` so callers can hold the token across an await without
    /// keeping the lock or copying the string per request.
    value: Arc<str>,
    expires_at: Instant,
}

impl CachedToken {
    fn is_usable(&self) -> bool {
        Instant::now() + EXPIRY_MARGIN < self.expires_at
    }
}

/// Mints and caches application access tokens.
#[derive(Debug)]
pub struct TokenProvider {
    http: reqwest::Client,
    token_url: String,
    client_id: String,
    client_secret: Secret,
    cached: RwLock<Option<CachedToken>>,
}

impl TokenProvider {
    #[must_use]
    pub fn new(
        http: reqwest::Client,
        token_url: String,
        client_id: String,
        client_secret: Secret,
    ) -> Self {
        Self {
            http,
            token_url,
            client_id,
            client_secret,
            cached: RwLock::new(None),
        }
    }

    /// Return a valid access token, minting a fresh one if the cache is empty
    /// or close to expiry.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] if eBay is unreachable or rejects the keyset.
    pub async fn access_token(&self) -> Result<Arc<str>, AuthError> {
        if let Some(token) = self.cached.read().await.as_ref()
            && token.is_usable()
        {
            return Ok(Arc::clone(&token.value));
        }

        // Holding the write lock across the request is deliberate: concurrent
        // tool calls that all observe an expired token queue up behind one
        // refresh instead of each firing their own.
        let mut guard = self.cached.write().await;
        if let Some(token) = guard.as_ref()
            && token.is_usable()
        {
            return Ok(Arc::clone(&token.value));
        }

        let fetched = self.request_token().await?;
        let value: Arc<str> = Arc::from(fetched.access_token);
        *guard = Some(CachedToken {
            value: Arc::clone(&value),
            expires_at: Instant::now() + Duration::from_secs(fetched.expires_in),
        });
        tracing::debug!(
            expires_in = fetched.expires_in,
            "minted eBay application token"
        );
        Ok(value)
    }

    /// Drop the cached token so the next call re-authenticates.
    ///
    /// Used when eBay answers a Browse request with 401 despite a token the
    /// cache still believes is fresh — for instance after a keyset rotation.
    pub async fn invalidate(&self) {
        *self.cached.write().await = None;
    }

    async fn request_token(&self) -> Result<TokenResponse, AuthError> {
        // eBay wants the keyset as HTTP Basic credentials rather than form
        // fields, per RFC 6749 §2.3.1.
        let basic = BASE64.encode(format!(
            "{}:{}",
            self.client_id,
            self.client_secret.expose()
        ));

        let response = self
            .http
            .post(&self.token_url)
            .header(reqwest::header::AUTHORIZATION, format!("Basic {basic}"))
            .form(&[("grant_type", "client_credentials"), ("scope", API_SCOPE)])
            .send()
            .await
            .map_err(AuthError::Transport)?;

        let status = response.status().as_u16();
        let body = response.text().await.map_err(AuthError::Transport)?;

        if let Ok(token) = serde_json::from_str::<TokenResponse>(&body) {
            return Ok(token);
        }
        if let Ok(err) = serde_json::from_str::<TokenErrorResponse>(&body) {
            return Err(AuthError::Rejected {
                status,
                error: err.error,
                description: err.error_description,
            });
        }
        Err(AuthError::Malformed {
            status,
            body: truncate(&body, 500),
        })
    }
}

/// Bound an untrusted upstream body before it lands in an error message,
/// stepping back to a character boundary so the slice cannot panic.
fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "panicking is how a test reports failure"
    )]

    use super::*;

    #[test]
    fn cached_token_expiring_within_the_margin_is_not_usable() {
        let token = CachedToken {
            value: Arc::from("t"),
            expires_at: Instant::now() + EXPIRY_MARGIN / 2,
        };
        assert!(!token.is_usable());
    }

    #[test]
    fn cached_token_well_ahead_of_expiry_is_usable() {
        let token = CachedToken {
            value: Arc::from("t"),
            expires_at: Instant::now() + EXPIRY_MARGIN * 4,
        };
        assert!(token.is_usable());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let text = "é".repeat(400);
        let truncated = truncate(&text, 500);
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() <= 504);
    }
}
