//! HTTP access to the Browse API.

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;

use super::{
    Marketplace,
    auth::{AuthError, TokenProvider},
    filter::{FilterError, ItemFieldGroup, SearchRequest},
    model::{ApiError, Item, ItemGroup, RawSearchResponse, SearchResults},
};
use crate::config::{Config, ContextualLocation};

/// eBay item ids embed `|` (`v1|254188828753|0`), which is not permitted raw in
/// a URL path. Encode it, along with the other characters that would change the
/// meaning of the path.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%')
    .add(b'|');

#[derive(Debug, thiserror::Error)]
pub enum BrowseError {
    #[error(transparent)]
    Auth(#[from] AuthError),

    #[error(transparent)]
    InvalidCriteria(#[from] FilterError),

    #[error("could not reach eBay: {0}")]
    Transport(#[source] reqwest::Error),

    #[error("no such item on eBay: {0}")]
    NotFound(String),

    #[error(
        "eBay rate-limited this application (HTTP 429). The Browse API allows a \
         fixed number of calls per day per keyset; wait before retrying"
    )]
    RateLimited,

    #[error("eBay rejected the request (HTTP {status}): {}", render_errors(errors))]
    Api { status: u16, errors: Vec<ApiError> },

    #[error("{method} does not support the {group} field group; {remedy}")]
    UnsupportedFieldGroup {
        group: &'static str,
        method: &'static str,
        remedy: &'static str,
    },

    #[error("eBay returned an unexpected response (HTTP {status}): {detail}")]
    Unexpected { status: u16, detail: String },
}

fn render_errors(errors: &[ApiError]) -> String {
    if errors.is_empty() {
        return "no detail supplied".to_owned();
    }
    errors
        .iter()
        .map(ApiError::summarize)
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Debug, serde::Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    errors: Vec<ApiError>,
}

/// A client for the subset of Browse API methods that an application token can
/// reach — that is, everything except the shopping-cart methods, which need a
/// token minted on behalf of a signed-in eBay user.
#[derive(Debug)]
pub struct BrowseClient {
    http: reqwest::Client,
    tokens: TokenProvider,
    base_url: String,
    default_marketplace: Marketplace,
    contextual_location: Option<ContextualLocation>,
    affiliate_campaign_id: Option<String>,
}

impl BrowseClient {
    /// Build a client from configuration.
    ///
    /// # Errors
    ///
    /// Returns [`reqwest::Error`] if the HTTP stack cannot be initialised —
    /// in practice, only a TLS backend failure.
    pub fn new(config: &Config) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()?;

        let tokens = TokenProvider::new(
            http.clone(),
            config.environment.token_url(),
            config.client_id.clone(),
            config.client_secret.clone(),
        );

        Ok(Self {
            http,
            tokens,
            base_url: config.environment.browse_base_url(),
            default_marketplace: config.default_marketplace,
            contextual_location: config.contextual_location.clone(),
            affiliate_campaign_id: config.affiliate_campaign_id.clone(),
        })
    }

    #[must_use]
    pub const fn default_marketplace(&self) -> Marketplace {
        self.default_marketplace
    }

    /// Keyword, category, and attribute search.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError`] if the criteria are invalid or eBay rejects the
    /// request.
    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResults, BrowseError> {
        let query = request.to_query_pairs()?;
        let marketplace = request.marketplace.unwrap_or(self.default_marketplace);
        let raw: RawSearchResponse = self
            .send(
                Method::GET,
                "item_summary/search",
                &query,
                None,
                marketplace,
            )
            .await?;
        Ok(raw.into())
    }

    /// Reverse image search.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError`] if the criteria are invalid, the image is not
    /// accepted, or eBay rejects the request.
    pub async fn search_by_image(
        &self,
        image_base64: &str,
        criteria: &SearchRequest,
    ) -> Result<SearchResults, BrowseError> {
        let query = criteria.to_image_query_pairs()?;
        let marketplace = criteria.marketplace.unwrap_or(self.default_marketplace);
        let body = serde_json::json!({ "image": image_base64 });
        let raw: RawSearchResponse = self
            .send(
                Method::POST,
                "item_summary/search_by_image",
                &query,
                Some(&body),
                marketplace,
            )
            .await?;
        Ok(raw.into())
    }

    /// Fetch one item by its RESTful id (`v1|254188828753|0`).
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError`] if the item does not exist or eBay rejects the
    /// request.
    pub async fn get_item(
        &self,
        item_id: &str,
        field_groups: &[ItemFieldGroup],
        marketplace: Option<Marketplace>,
    ) -> Result<Item, BrowseError> {
        let path = format!("item/{}", encode_segment(item_id));
        let query = field_group_query(field_groups);
        self.send(
            Method::GET,
            &path,
            &query,
            None,
            marketplace.unwrap_or(self.default_marketplace),
        )
        .await
        .map_err(|err| specialize_not_found(err, item_id))
    }

    /// Fetch one item by the numeric listing id shown in an ebay.com URL.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError`] if the item does not exist or eBay rejects the
    /// request.
    pub async fn get_item_by_legacy_id(
        &self,
        legacy_item_id: &str,
        field_groups: &[ItemFieldGroup],
        marketplace: Option<Marketplace>,
    ) -> Result<Item, BrowseError> {
        reject_compact(field_groups)?;
        let mut query = vec![("legacy_item_id", legacy_item_id.to_owned())];
        query.extend(field_group_query(field_groups));
        self.send(
            Method::GET,
            "item/get_item_by_legacy_id",
            &query,
            None,
            marketplace.unwrap_or(self.default_marketplace),
        )
        .await
        .map_err(|err| specialize_not_found(err, legacy_item_id))
    }

    /// Fetch every variation of a multi-variation listing.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError`] if the group does not exist or eBay rejects the
    /// request.
    pub async fn get_items_by_item_group(
        &self,
        item_group_id: &str,
        marketplace: Option<Marketplace>,
    ) -> Result<ItemGroup, BrowseError> {
        let query = vec![("item_group_id", item_group_id.to_owned())];
        self.send(
            Method::GET,
            "item/get_items_by_item_group",
            &query,
            None,
            marketplace.unwrap_or(self.default_marketplace),
        )
        .await
        .map_err(|err| specialize_not_found(err, item_group_id))
    }

    /// Issue an authenticated request, retrying once if the cached token turns
    /// out to have been revoked server-side.
    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&'static str, String)],
        body: Option<&serde_json::Value>,
        marketplace: Marketplace,
    ) -> Result<T, BrowseError> {
        let url = format!("{}/{path}", self.base_url);

        for attempt in 0..2u8 {
            let token = self.tokens.access_token().await?;

            let mut request = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(token.as_ref())
                .header("X-EBAY-C-MARKETPLACE-ID", marketplace.as_header_value())
                .header(reqwest::header::ACCEPT, "application/json")
                .query(query);

            if let Some(context) = self.end_user_context() {
                request = request.header("X-EBAY-C-ENDUSERCTX", context);
            }
            if let Some(body) = body {
                request = request.json(body);
            }

            let response = request.send().await.map_err(BrowseError::Transport)?;
            let status = response.status();

            // A 401 against a token the cache believed was fresh means the
            // token died early — a revoked or rotated keyset. Re-mint once
            // before surfacing the failure.
            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                tracing::warn!("eBay rejected the cached token; re-authenticating");
                self.tokens.invalidate().await;
                continue;
            }

            return Self::decode(status, response).await;
        }

        Err(BrowseError::Unexpected {
            status: StatusCode::UNAUTHORIZED.as_u16(),
            detail: "authentication kept failing after re-minting the token".to_owned(),
        })
    }

    async fn decode<T: DeserializeOwned>(
        status: StatusCode,
        response: reqwest::Response,
    ) -> Result<T, BrowseError> {
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(BrowseError::RateLimited);
        }

        let body = response.text().await.map_err(BrowseError::Transport)?;

        if status.is_success() {
            return serde_json::from_str(&body).map_err(|err| BrowseError::Unexpected {
                status: status.as_u16(),
                detail: format!("could not decode the response: {err}"),
            });
        }

        match serde_json::from_str::<ErrorEnvelope>(&body) {
            Ok(envelope) if !envelope.errors.is_empty() => Err(BrowseError::Api {
                status: status.as_u16(),
                errors: envelope.errors,
            }),
            _ => Err(BrowseError::Unexpected {
                status: status.as_u16(),
                detail: truncate(&body, 500),
            }),
        }
    }

    /// eBay reads buyer location and affiliate attribution from a single
    /// comma-delimited header.
    fn end_user_context(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(location) = &self.contextual_location {
            parts.push(format!("contextualLocation={}", location.to_header_value()));
        }
        if let Some(campaign) = &self.affiliate_campaign_id {
            parts.push(format!("affiliateCampaignId={campaign}"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(","))
        }
    }
}

/// eBay answers a missing item with 404 and a generic message; restate it with
/// the identifier that was actually asked for.
fn specialize_not_found(error: BrowseError, identifier: &str) -> BrowseError {
    match error {
        BrowseError::Api { status: 404, .. } => BrowseError::NotFound(identifier.to_owned()),
        other => other,
    }
}

/// COMPACT is the one field group the legacy-id lookup does not implement.
/// eBay's rejection does not say which parameter is at fault, so catch it here
/// and name the alternative.
fn reject_compact(groups: &[ItemFieldGroup]) -> Result<(), BrowseError> {
    if groups.contains(&ItemFieldGroup::Compact) {
        return Err(BrowseError::UnsupportedFieldGroup {
            group: "COMPACT",
            method: "lookup by numeric listing id",
            remedy: "fetch the item by its RESTful `itemId` with get_ebay_item instead",
        });
    }
    Ok(())
}

fn field_group_query(groups: &[ItemFieldGroup]) -> Vec<(&'static str, String)> {
    if groups.is_empty() {
        return Vec::new();
    }
    vec![("fieldgroups", ItemFieldGroup::join(groups))]
}

fn encode_segment(segment: &str) -> String {
    utf8_percent_encode(segment, PATH_SEGMENT).to_string()
}

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
    fn item_ids_are_path_safe() {
        assert_eq!(encode_segment("v1|254188828753|0"), "v1%7C254188828753%7C0");
    }

    #[test]
    fn error_rendering_joins_every_complaint() {
        let errors = vec![
            ApiError {
                error_id: Some(12001),
                domain: None,
                category: None,
                message: None,
                long_message: Some("bad filter".to_owned()),
                parameters: vec![],
            },
            ApiError {
                error_id: Some(12002),
                domain: None,
                category: None,
                message: Some("bad sort".to_owned()),
                long_message: None,
                parameters: vec![],
            },
        ];
        assert_eq!(
            render_errors(&errors),
            "[12001] bad filter; [12002] bad sort"
        );
    }

    #[test]
    fn the_legacy_lookup_refuses_compact_with_a_way_forward() {
        let err = reject_compact(&[ItemFieldGroup::Product, ItemFieldGroup::Compact]).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("COMPACT"), "{message}");
        assert!(message.contains("get_ebay_item"), "{message}");
    }

    #[test]
    fn the_legacy_lookup_allows_every_other_field_group() {
        assert!(
            reject_compact(&[
                ItemFieldGroup::Product,
                ItemFieldGroup::AdditionalSellerDetails,
                ItemFieldGroup::CharityDetails,
            ])
            .is_ok()
        );
    }

    #[test]
    fn field_groups_render_as_a_comma_separated_list() {
        let query = field_group_query(&[ItemFieldGroup::Product, ItemFieldGroup::CharityDetails]);
        assert_eq!(
            query,
            vec![("fieldgroups", "PRODUCT,CHARITY_DETAILS".to_owned())]
        );
        assert!(field_group_query(&[]).is_empty());
    }

    #[test]
    fn empty_error_list_still_says_something() {
        assert_eq!(render_errors(&[]), "no detail supplied");
    }
}
