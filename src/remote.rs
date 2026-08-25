//! Remote transport: the axum router that wraps the MCP Streamable HTTP
//! service with the OAuth provider from [`crate::oauth`].
//!
//! Everything lives on one origin: the two discovery documents, the
//! authorization endpoints, and the MCP endpoint itself behind a bearer
//! gate. Claude's discovery starts from a 401 — it ignores
//! `WWW-Authenticate` on a 200 — so the gate always answers with the
//! challenge shape from RFC 9728 §5.1.
//!
//! TLS is deliberately out of scope: the binary serves plain HTTP and
//! expects a reverse proxy to terminate HTTPS for the public hostname.

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    Form, Json, Router,
    extract::{Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;

use crate::oauth::{OauthCore, OauthError, TokenGrant};

type Core = Arc<OauthCore>;

/// Builds the public router. `mcp_service` is generic so tests can gate a
/// stub instead of a real MCP transport (constructing one would build a
/// `reqwest::Client`, which the Nix sandbox cannot do).
pub fn router<S>(core: Core, mcp_path: &str, mcp_service: S) -> Router
where
    S: tower::Service<Request, Error = Infallible> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse + 'static,
    S::Future: Send + 'static,
{
    let gated = Router::new()
        .route_service(mcp_path, mcp_service)
        .route_layer(middleware::from_fn_with_state(core.clone(), bearer_gate));
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        // Claude probes the path-suffixed RFC 9728 variant first
        // (`…/oauth-protected-resource/mcp` for a server at `/mcp`).
        .route(
            "/.well-known/oauth-protected-resource/{*suffix}",
            get(protected_resource_metadata),
        )
        .route("/authorize", get(authorize_page).post(authorize_submit))
        .route("/token", post(token))
        .route("/register", post(register))
        .with_state(core)
        .merge(gated)
}

async fn authorization_server_metadata(State(core): State<Core>) -> Json<serde_json::Value> {
    Json(json!({
        "issuer": core.issuer,
        "authorization_endpoint": format!("{}/authorize", core.issuer),
        "token_endpoint": format!("{}/token", core.issuer),
        "registration_endpoint": format!("{}/register", core.issuer),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        // Advertising offline_access is what makes Claude ask for a
        // refresh token.
        "scopes_supported": ["offline_access"],
    }))
}

async fn protected_resource_metadata(State(core): State<Core>) -> Json<serde_json::Value> {
    Json(json!({
        "resource": core.resource,
        "authorization_servers": [core.issuer],
        "bearer_methods_supported": ["header"],
    }))
}

/// Rejects anything without a valid bearer token, with the 401 challenge
/// that seeds Claude's metadata discovery.
async fn bearer_gate(State(core): State<Core>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then_some(token)
        });
    let rejection = match presented {
        None => None,
        Some(token) => match core.validate_bearer(token) {
            Ok(()) => return next.run(request).await,
            Err(err) => Some(err.to_string()),
        },
    };
    let mut challenge = format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
        core.issuer
    );
    if let Some(description) = rejection {
        challenge.push_str(&format!(
            ", error=\"invalid_token\", error_description=\"{description}\""
        ));
    }
    let header_value =
        HeaderValue::from_str(&challenge).unwrap_or_else(|_| HeaderValue::from_static("Bearer"));
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, header_value)],
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct RegisterRequest {
    #[serde(default)]
    redirect_uris: Vec<String>,
}

async fn register(State(core): State<Core>, Json(request): Json<RegisterRequest>) -> Response {
    match core.register_client(&request.redirect_uris) {
        Ok(client) => (
            StatusCode::CREATED,
            Json(json!({
                "client_id": client.client_id,
                "client_id_issued_at": jiff::Timestamp::now().as_second(),
                "redirect_uris": client.redirect_uris,
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "invalid_redirect_uri",
                "error_description": err.to_string(),
            })),
        )
            .into_response(),
    }
}

/// Params shared by the authorization page and its form submission. All
/// optional so validation can produce proper OAuth errors instead of
/// extractor rejections.
#[derive(Deserialize)]
struct AuthorizeParams {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    scope: Option<String>,
    resource: Option<String>,
    /// Present only on the consent form submission.
    access_key: Option<String>,
}

/// The validated subset of an authorization request that later steps need.
struct AuthorizeRequest<'a> {
    client_id: &'a str,
    redirect_uri: &'a str,
    state: Option<&'a str>,
    code_challenge: &'a str,
    scope: Option<&'a str>,
}

enum AuthorizeOutcome<'a> {
    Proceed(AuthorizeRequest<'a>),
    /// No trusted redirect target; render the error, never redirect.
    Reject(String),
    /// Redirect target is trusted; report the error through it.
    ErrorRedirect(Response),
}

fn validate_authorize<'a>(core: &OauthCore, params: &'a AuthorizeParams) -> AuthorizeOutcome<'a> {
    let (Some(client_id), Some(redirect_uri)) =
        (params.client_id.as_deref(), params.redirect_uri.as_deref())
    else {
        return AuthorizeOutcome::Reject("client_id and redirect_uri are required".to_owned());
    };
    if let Err(err) = core.check_authorize_target(client_id, redirect_uri) {
        return AuthorizeOutcome::Reject(err.to_string());
    }
    let state = params.state.as_deref();
    let bounce = |error: &str, description: &str| {
        AuthorizeOutcome::ErrorRedirect(error_redirect(
            core,
            redirect_uri,
            error,
            description,
            state,
        ))
    };
    if params.response_type.as_deref() != Some("code") {
        return bounce(
            "unsupported_response_type",
            "only response_type=code is supported",
        );
    }
    if let Some(resource) = params.resource.as_deref()
        && resource != core.resource
    {
        return bounce(
            "invalid_target",
            "resource does not match this server's resource identifier",
        );
    }
    if params.code_challenge_method.as_deref() != Some("S256") {
        return bounce("invalid_request", "code_challenge_method must be S256");
    }
    let Some(code_challenge) = params
        .code_challenge
        .as_deref()
        .filter(|challenge| (43..=128).contains(&challenge.len()))
    else {
        return bounce(
            "invalid_request",
            "a code_challenge of 43-128 characters is required",
        );
    };
    AuthorizeOutcome::Proceed(AuthorizeRequest {
        client_id,
        redirect_uri,
        state,
        code_challenge,
        scope: params.scope.as_deref(),
    })
}

async fn authorize_page(
    State(core): State<Core>,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    match validate_authorize(&core, &params) {
        AuthorizeOutcome::Reject(reason) => invalid_authorize_page(&reason),
        AuthorizeOutcome::ErrorRedirect(response) => response,
        AuthorizeOutcome::Proceed(_) => consent_page(&params, None),
    }
}

async fn authorize_submit(
    State(core): State<Core>,
    Form(params): Form<AuthorizeParams>,
) -> Response {
    // Hidden form fields are as attacker-controlled as query parameters, so
    // the submission is validated from scratch.
    match validate_authorize(&core, &params) {
        AuthorizeOutcome::Reject(reason) => invalid_authorize_page(&reason),
        AuthorizeOutcome::ErrorRedirect(response) => response,
        AuthorizeOutcome::Proceed(request) => {
            if !params
                .access_key
                .as_deref()
                .is_some_and(|key| core.access_key_matches(key))
            {
                return consent_page(&params, Some("Wrong access key."));
            }
            let code = match core.mint_code(
                request.client_id,
                request.redirect_uri,
                request.code_challenge,
                request.scope,
            ) {
                Ok(code) => code,
                Err(err) => return invalid_authorize_page(&err.to_string()),
            };
            let mut pairs = vec![("code", code)];
            if let Some(state) = request.state {
                pairs.push(("state", state.to_owned()));
            }
            // RFC 9207: identify the issuer in the response.
            pairs.push(("iss", core.issuer.clone()));
            match append_query(request.redirect_uri, &pairs) {
                Some(location) => Redirect::to(&location).into_response(),
                None => invalid_authorize_page("redirect_uri could not be parsed"),
            }
        }
    }
}

#[derive(Deserialize)]
struct TokenParams {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
}

async fn token(State(core): State<Core>, Form(params): Form<TokenParams>) -> Response {
    let result = grant_for(&core, &params);
    match result {
        Ok(grant) => {
            let mut body = json!({
                "access_token": grant.access_token,
                "token_type": "Bearer",
                "expires_in": grant.expires_in,
                "refresh_token": grant.refresh_token,
            });
            if let (Some(scope), Some(object)) = (grant.scope, body.as_object_mut()) {
                object.insert("scope".to_owned(), json!(scope));
            }
            (
                StatusCode::OK,
                // RFC 6749 §5.1 requires token responses to be uncacheable.
                [
                    (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
                    (header::PRAGMA, HeaderValue::from_static("no-cache")),
                ],
                Json(body),
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": err.code(),
                "error_description": err.to_string(),
            })),
        )
            .into_response(),
    }
}

fn grant_for(core: &OauthCore, params: &TokenParams) -> Result<TokenGrant, OauthError> {
    if let Some(resource) = params.resource.as_deref()
        && resource != core.resource
    {
        return Err(OauthError::InvalidTarget);
    }
    let need = |field: &str, value: &Option<String>| -> Result<String, OauthError> {
        value
            .clone()
            .ok_or_else(|| OauthError::InvalidRequest(format!("{field} is required")))
    };
    match params.grant_type.as_deref() {
        Some("authorization_code") => core.redeem_code(
            &need("code", &params.code)?,
            &need("client_id", &params.client_id)?,
            &need("redirect_uri", &params.redirect_uri)?,
            &need("code_verifier", &params.code_verifier)?,
        ),
        Some("refresh_token") => core.refresh(
            &need("refresh_token", &params.refresh_token)?,
            &need("client_id", &params.client_id)?,
        ),
        _ => Err(OauthError::UnsupportedGrantType),
    }
}

fn error_redirect(
    core: &OauthCore,
    redirect_uri: &str,
    error: &str,
    description: &str,
    state: Option<&str>,
) -> Response {
    let mut pairs = vec![
        ("error", error.to_owned()),
        ("error_description", description.to_owned()),
    ];
    if let Some(state) = state {
        pairs.push(("state", state.to_owned()));
    }
    pairs.push(("iss", core.issuer.clone()));
    match append_query(redirect_uri, &pairs) {
        Some(location) => Redirect::to(&location).into_response(),
        None => invalid_authorize_page("redirect_uri could not be parsed"),
    }
}

fn append_query(redirect_uri: &str, pairs: &[(&str, String)]) -> Option<String> {
    let mut url = url::Url::parse(redirect_uri).ok()?;
    for (key, value) in pairs {
        url.query_pairs_mut().append_pair(key, value);
    }
    Some(url.into())
}

/// 400 page for requests with no trustworthy redirect target. Shown to a
/// human mid-flow, so it explains itself.
fn invalid_authorize_page(reason: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(format!(
            "{PAGE_HEAD}<h1>eBay Browse MCP</h1>\
             <p>This authorization request is invalid and cannot be completed.</p>\
             <p class=\"error\">{}</p>{PAGE_FOOT}",
            escape_html(reason)
        )),
    )
        .into_response()
}

fn consent_page(params: &AuthorizeParams, error: Option<&str>) -> Response {
    let mut hidden = String::new();
    for (name, value) in [
        ("response_type", &params.response_type),
        ("client_id", &params.client_id),
        ("redirect_uri", &params.redirect_uri),
        ("state", &params.state),
        ("code_challenge", &params.code_challenge),
        ("code_challenge_method", &params.code_challenge_method),
        ("scope", &params.scope),
        ("resource", &params.resource),
    ] {
        if let Some(value) = value {
            hidden.push_str(&format!(
                "<input type=\"hidden\" name=\"{name}\" value=\"{}\">",
                escape_html(value)
            ));
        }
    }
    // RFC 8252 §8.4 / MCP spec: show where the user will be sent, so a
    // spoofed loopback client is at least visible on the consent screen.
    let destination = params
        .redirect_uri
        .as_deref()
        .map(escape_html)
        .unwrap_or_default();
    let banner = error
        .map(|message| format!("<p class=\"error\">{}</p>", escape_html(message)))
        .unwrap_or_default();
    Html(format!(
        "{PAGE_HEAD}<h1>eBay Browse MCP</h1>\
         <p>A client is asking to connect. After approval it will be able to \
         search eBay listings through this server.</p>\
         <p>It will be sent back to:</p><p class=\"dest\">{destination}</p>{banner}\
         <form method=\"post\" action=\"authorize\">{hidden}\
         <label for=\"access_key\">Access key</label>\
         <input type=\"password\" id=\"access_key\" name=\"access_key\" \
         autofocus required autocomplete=\"current-password\">\
         <button type=\"submit\">Approve</button></form>{PAGE_FOOT}"
    ))
    .into_response()
}

const PAGE_HEAD: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>eBay Browse MCP</title><style>\
:root{color-scheme:light dark}\
body{font-family:system-ui,sans-serif;max-width:26rem;margin:15vh auto 0;padding:0 1rem;\
line-height:1.5}\
h1{font-size:1.25rem}\
.dest{font-family:monospace;word-break:break-all;padding:.5rem;border-radius:.375rem;\
background:color-mix(in srgb,currentcolor 8%,transparent)}\
.error{color:#c0392b}\
label{display:block;margin-top:1rem}\
input[type=password]{display:block;width:100%;box-sizing:border-box;margin:.375rem 0 1rem;\
padding:.5rem;font-size:1rem}\
button{padding:.5rem 1.5rem;font-size:1rem}\
</style></head><body>";

const PAGE_FOOT: &str = "</body></html>";

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test assertions are clearer unwrapped"
    )]

    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode, header};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    use tower::ServiceExt as _;

    use super::*;
    use crate::config::Secret;
    use crate::oauth::CLAUDE_CALLBACK;

    const ACCESS_KEY: &str = "test-access-key-for-remote";

    fn test_router() -> Router {
        let core = Arc::new(OauthCore::new(
            "https://mcp.example.test".to_owned(),
            "https://mcp.example.test/mcp".to_owned(),
            Secret::new("0123456789abcdef0123456789abcdef"),
            Secret::new(ACCESS_KEY),
        ));
        let stub = tower::service_fn(|_request: Request| async {
            Ok::<_, Infallible>((StatusCode::OK, "mcp-ok").into_response())
        });
        router(core, "/mcp", stub)
    }

    async fn send(router: &Router, request: HttpRequest<Body>) -> (StatusCode, String, Response) {
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        (
            status,
            String::from_utf8_lossy(&bytes).into_owned(),
            Response::from_parts(parts, Body::empty()),
        )
    }

    async fn get_json(router: &Router, uri: &str) -> serde_json::Value {
        let (status, body, _) =
            send(router, HttpRequest::get(uri).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        serde_json::from_str(&body).unwrap()
    }

    fn form_request(uri: &str, body: String) -> HttpRequest<Body> {
        HttpRequest::post(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    }

    async fn register_client(router: &Router) -> String {
        let (status, body, _) = send(
            router,
            HttpRequest::post("/register")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "redirect_uris": [CLAUDE_CALLBACK] })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let response: serde_json::Value = serde_json::from_str(&body).unwrap();
        response["client_id"].as_str().unwrap().to_owned()
    }

    /// Runs consent + code exchange, returning the token response.
    async fn obtain_grant(router: &Router, client_id: &str, verifier: &str) -> serde_json::Value {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let consent = [
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", CLAUDE_CALLBACK),
            ("state", "st-123"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("access_key", ACCESS_KEY),
        ];
        let body = serde_urlencoded(&consent);
        let (status, _, response) = send(router, form_request("/authorize", body)).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let redirect = url::Url::parse(&location).unwrap();
        assert!(location.starts_with(CLAUDE_CALLBACK));
        let query: std::collections::HashMap<_, _> = redirect.query_pairs().collect();
        assert_eq!(query["state"], "st-123");
        assert_eq!(query["iss"], "https://mcp.example.test");
        let code = query["code"].to_string();

        let exchange = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", CLAUDE_CALLBACK),
            ("client_id", client_id),
            ("code_verifier", verifier),
        ];
        let (status, body, response) =
            send(router, form_request("/token", serde_urlencoded(&exchange))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        serde_json::from_str(&body).unwrap()
    }

    fn serde_urlencoded(pairs: &[(&str, &str)]) -> String {
        let mut url = url::Url::parse("http://x/").unwrap();
        url.query_pairs_mut().extend_pairs(pairs);
        url.query().unwrap_or_default().to_owned()
    }

    #[tokio::test]
    async fn protected_resource_metadata_matches_rfc9728() {
        let router = test_router();
        for uri in [
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ] {
            let metadata = get_json(&router, uri).await;
            assert_eq!(metadata["resource"], "https://mcp.example.test/mcp");
            assert_eq!(
                metadata["authorization_servers"][0],
                "https://mcp.example.test"
            );
        }
    }

    #[tokio::test]
    async fn authorization_server_metadata_advertises_required_support() {
        let router = test_router();
        let metadata = get_json(&router, "/.well-known/oauth-authorization-server").await;
        assert_eq!(metadata["issuer"], "https://mcp.example.test");
        assert_eq!(
            metadata["authorization_endpoint"],
            "https://mcp.example.test/authorize"
        );
        assert_eq!(metadata["token_endpoint"], "https://mcp.example.test/token");
        assert_eq!(
            metadata["registration_endpoint"],
            "https://mcp.example.test/register"
        );
        assert_eq!(metadata["code_challenge_methods_supported"][0], "S256");
        assert_eq!(metadata["token_endpoint_auth_methods_supported"][0], "none");
        assert_eq!(metadata["scopes_supported"][0], "offline_access");
    }

    #[tokio::test]
    async fn unauthenticated_mcp_request_draws_the_discovery_challenge() {
        let router = test_router();
        let (status, _, response) = send(
            &router,
            HttpRequest::post("/mcp").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(challenge.starts_with("Bearer resource_metadata=\""));
        assert!(
            challenge.contains("https://mcp.example.test/.well-known/oauth-protected-resource")
        );
        // No token was presented, so no error attribute.
        assert!(!challenge.contains("error="));
    }

    #[tokio::test]
    async fn garbage_bearer_token_is_rejected_with_invalid_token() {
        let router = test_router();
        let (status, _, response) = send(
            &router,
            HttpRequest::post("/mcp")
                .header(header::AUTHORIZATION, "Bearer nonsense")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(challenge.contains("error=\"invalid_token\""));
    }

    #[tokio::test]
    async fn full_flow_reaches_the_mcp_service() {
        let router = test_router();
        let client_id = register_client(&router).await;
        let tokens = obtain_grant(
            &router,
            &client_id,
            "verifier-verifier-verifier-verifier-1234",
        )
        .await;
        assert_eq!(tokens["token_type"], "Bearer");
        assert!(tokens["refresh_token"].is_string());

        let (status, body, _) = send(
            &router,
            HttpRequest::post("/mcp")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", tokens["access_token"].as_str().unwrap()),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "mcp-ok");
    }

    #[tokio::test]
    async fn consent_page_renders_for_a_valid_request() {
        let router = test_router();
        let client_id = register_client(&router).await;
        let challenge = "c".repeat(43);
        let query = serde_urlencoded(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", CLAUDE_CALLBACK),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);
        let (status, body, _) = send(
            &router,
            HttpRequest::get(format!("/authorize?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("name=\"access_key\""));
        assert!(body.contains("claude.ai"));
    }

    #[tokio::test]
    async fn wrong_access_key_rerenders_instead_of_redirecting() {
        let router = test_router();
        let client_id = register_client(&router).await;
        let challenge = "c".repeat(43);
        let consent = [
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", CLAUDE_CALLBACK),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("access_key", "not-the-key"),
        ];
        let (status, body, _) = send(
            &router,
            form_request("/authorize", serde_urlencoded(&consent)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Wrong access key."));
    }

    #[tokio::test]
    async fn unregistered_redirect_is_never_redirected_to() {
        let router = test_router();
        let client_id = register_client(&router).await;
        let challenge = "c".repeat(43);
        let query = serde_urlencoded(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://evil.example/steal"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);
        let (status, _, response) = send(
            &router,
            HttpRequest::get(format!("/authorize?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response.headers().get(header::LOCATION).is_none());
    }

    #[tokio::test]
    async fn code_replay_is_rejected() {
        let router = test_router();
        let client_id = register_client(&router).await;
        let verifier = "verifier-verifier-verifier-verifier-5678";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let consent = [
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", CLAUDE_CALLBACK),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("access_key", ACCESS_KEY),
        ];
        let (_, _, response) = send(
            &router,
            form_request("/authorize", serde_urlencoded(&consent)),
        )
        .await;
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let redirect = url::Url::parse(&location).unwrap();
        let code = redirect
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        let exchange = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", CLAUDE_CALLBACK),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier),
        ];
        let (status, _, _) =
            send(&router, form_request("/token", serde_urlencoded(&exchange))).await;
        assert_eq!(status, StatusCode::OK);
        let (status, body, _) =
            send(&router, form_request("/token", serde_urlencoded(&exchange))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid_grant"));
    }

    #[tokio::test]
    async fn refresh_grant_rotates_tokens() {
        let router = test_router();
        let client_id = register_client(&router).await;
        let tokens = obtain_grant(
            &router,
            &client_id,
            "verifier-verifier-verifier-verifier-9999",
        )
        .await;
        let refresh = [
            ("grant_type", "refresh_token"),
            ("refresh_token", tokens["refresh_token"].as_str().unwrap()),
            ("client_id", client_id.as_str()),
        ];
        let (status, body, _) =
            send(&router, form_request("/token", serde_urlencoded(&refresh))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let rotated: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_ne!(rotated["access_token"], tokens["access_token"]);
        assert_ne!(rotated["refresh_token"], tokens["refresh_token"]);
    }

    #[tokio::test]
    async fn unknown_grant_type_is_rejected() {
        let router = test_router();
        let (status, body, _) = send(
            &router,
            form_request(
                "/token",
                serde_urlencoded(&[("grant_type", "client_credentials")]),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("unsupported_grant_type"));
    }

    #[tokio::test]
    async fn token_endpoint_requires_form_encoding() {
        let router = test_router();
        let (status, _, _) = send(
            &router,
            HttpRequest::post("/token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"grant_type\":\"authorization_code\"}"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}
