//! eBay Marketplace Account Deletion/Closure notification endpoint.
//!
//! A production keyset must subscribe to these notifications (or obtain an
//! exemption) before eBay grants API access. The contract has two halves:
//!
//! - **Challenge**: eBay `GET`s the registered URL with a `challenge_code`
//!   query parameter and expects `{"challengeResponse": <hex>}`, where the
//!   hex is SHA-256 over challenge code + verification token + endpoint URL,
//!   in that order.
//! - **Notification**: eBay `POST`s a JSON notification per deleted account
//!   and accepts 200/201/202/204 as acknowledgment. An endpoint that fails
//!   to acknowledge for 24 hours is marked down, and the keyset drifts
//!   toward non-compliance.
//!
//! This server persists no per-user eBay data — search results stream
//! through to the MCP client — so there is nothing to delete; notifications
//! are acknowledged unconditionally and logged as the audit trail.
//! Verifying the `x-ebay-signature` header is documented as recommended
//! rather than required and is deliberately omitted: a forged notification
//! triggers nothing but a log line.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::Secret;

/// Path the endpoint is mounted at, on the same origin as the OAuth
/// provider. The full URL is logged at startup and must be registered
/// verbatim in the eBay developer portal.
pub const ENDPOINT_PATH: &str = "/ebay/account-deletion";

pub struct AccountDeletion {
    verification_token: Secret,
    /// A challenge-hash input, so it must byte-match the URL registered
    /// with eBay. Deriving it from the issuer origin keeps it from drifting
    /// away from where the route is actually mounted.
    endpoint_url: String,
}

impl AccountDeletion {
    #[must_use]
    pub fn new(issuer: &str, verification_token: Secret) -> Self {
        Self {
            verification_token,
            endpoint_url: format!("{issuer}{ENDPOINT_PATH}"),
        }
    }

    /// The exact URL to enter in the eBay developer portal.
    #[must_use]
    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }
}

/// Routes for the endpoint. eBay's calls are unauthenticated, so the caller
/// merges this outside the bearer gate.
pub fn router(endpoint: Arc<AccountDeletion>) -> Router {
    Router::new()
        .route(ENDPOINT_PATH, get(challenge).post(notification))
        .with_state(endpoint)
}

#[derive(Deserialize)]
struct ChallengeParams {
    challenge_code: Option<String>,
}

async fn challenge(
    State(endpoint): State<Arc<AccountDeletion>>,
    Query(params): Query<ChallengeParams>,
) -> Response {
    let Some(code) = params.challenge_code else {
        return (
            StatusCode::BAD_REQUEST,
            "the challenge_code query parameter is required",
        )
            .into_response();
    };
    tracing::info!("answering eBay account-deletion endpoint challenge");
    Json(serde_json::json!({
        "challengeResponse": challenge_response(
            &code,
            endpoint.verification_token.expose(),
            &endpoint.endpoint_url,
        ),
    }))
    .into_response()
}

/// SHA-256 over the three values in eBay's documented order, lowercase hex.
fn challenge_response(
    challenge_code: &str,
    verification_token: &str,
    endpoint_url: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(challenge_code);
    hasher.update(verification_token);
    hasher.update(endpoint_url);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The subset of a notification worth logging. Every field is optional so a
/// schema change on eBay's side degrades the log line, not the ack.
#[derive(Deserialize)]
struct Envelope {
    metadata: Option<Metadata>,
    notification: Option<Notification>,
}

#[derive(Default, Deserialize)]
struct Metadata {
    topic: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Notification {
    notification_id: Option<String>,
    event_date: Option<String>,
    publish_attempt_count: Option<u64>,
    data: Option<UserData>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserData {
    username: Option<String>,
    user_id: Option<String>,
}

/// Nothing is stored per eBay user, so deletion is complete on receipt and
/// the log line is the audit trail. The body is taken as raw bytes rather
/// than `Json<T>` because acknowledgment must not hinge on parseability —
/// unacknowledged notifications get the endpoint marked down.
async fn notification(body: Bytes) -> StatusCode {
    match serde_json::from_slice::<Envelope>(&body) {
        Ok(envelope) => {
            let metadata = envelope.metadata.unwrap_or_default();
            let notification = envelope.notification.unwrap_or_default();
            let data = notification.data.unwrap_or_default();
            tracing::info!(
                topic = ?metadata.topic,
                notification_id = ?notification.notification_id,
                event_date = ?notification.event_date,
                publish_attempt = ?notification.publish_attempt_count,
                username = ?data.username,
                user_id = ?data.user_id,
                "acknowledged eBay marketplace-account-deletion notification"
            );
        }
        Err(error) => {
            tracing::warn!(
                %error,
                bytes = body.len(),
                "acknowledged an unparseable POST to the account-deletion endpoint"
            );
        }
    }
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions are clearer unwrapped")]

    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt as _;

    use super::*;

    const TOKEN: &str = "QWERTYqwerty1234567890-_QWERTYqwerty123456";
    const CHALLENGE_CODE: &str = "a8628072-3d33-45ee-9004-bd65a5ab7d6d";
    /// Computed independently with coreutils `sha256sum` over
    /// code + token + endpoint URL, so the test does not merely re-run the
    /// code under test.
    const EXPECTED: &str = "30f63e1021d1d703bc844b48ac4a951a6acfc51ed16e66a5effb3953ef2c0a19";

    fn test_router() -> Router {
        router(Arc::new(AccountDeletion::new(
            "https://mcp.example.test",
            Secret::new(TOKEN),
        )))
    }

    #[test]
    fn challenge_hash_matches_independent_vector() {
        assert_eq!(
            challenge_response(
                CHALLENGE_CODE,
                TOKEN,
                "https://mcp.example.test/ebay/account-deletion"
            ),
            EXPECTED
        );
    }

    #[tokio::test]
    async fn challenge_is_answered_as_json() {
        let response = test_router()
            .oneshot(
                Request::get(format!(
                    "/ebay/account-deletion?challenge_code={CHALLENGE_CODE}"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["challengeResponse"], EXPECTED);
    }

    #[tokio::test]
    async fn challenge_without_code_is_rejected() {
        let response = test_router()
            .oneshot(
                Request::get("/ebay/account-deletion")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn notification_is_acknowledged() {
        let payload = serde_json::json!({
            "metadata": {
                "topic": "MARKETPLACE_ACCOUNT_DELETION",
                "schemaVersion": "1.0",
                "deprecated": false,
            },
            "notification": {
                "notificationId": "49feeaeb-4982-42d9-a377-9645b8479411",
                "eventDate": "2021-03-19T20:43:59.462Z",
                "publishDate": "2021-03-19T20:43:59.679Z",
                "publishAttemptCount": 1,
                "data": {
                    "username": "test_user",
                    "userId": "ma8vp1jySJC",
                    "eiasToken": "nY+sHZ2PrBmdj6wVnY+sEZ2PrA2dj6wJnY+gAZGEpQmdj6x9nY+seQ==",
                },
            },
        });
        let response = test_router()
            .oneshot(
                Request::post("/ebay/account-deletion")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unparseable_notification_is_still_acknowledged() {
        let response = test_router()
            .oneshot(
                Request::post("/ebay/account-deletion")
                    .body(Body::from("not json at all"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
