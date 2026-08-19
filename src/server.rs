//! The MCP surface: tools an agent can call, and their mapping onto the
//! Browse API.

use std::sync::Arc;

use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    config::Config,
    ebay::{
        BrowseClient, Marketplace,
        client::BrowseError,
        filter::{ItemFieldGroup, SearchRequest},
        model::{Item, ItemGroup, SearchResults},
    },
};

/// Guidance handed to the model at connection time. Worth the words: it turns
/// a pile of parameters into a workflow, and pre-empts the two mistakes that
/// otherwise cost a round trip each — filtering on a price without a currency,
/// and guessing at aspect names instead of discovering them.
const INSTRUCTIONS: &str = "\
Search eBay listings through the Buy Browse API.

Typical flow:
  1. `search_ebay` with keywords. For a broad or unfamiliar query, ask for
     `field_groups: [\"CategoryRefinements\", \"AspectRefinements\"]` — the
     matching listings still come back alongside the facets.
  2. Read the returned `refinements` to learn the real category ids and aspect
     names/values for this product area, then search again with
     `category_ids` and `aspects` to narrow precisely. Aspect names are
     category-specific and must be spelled exactly as eBay returns them —
     do not guess them.
  3. Call `get_ebay_item` with an `itemId` from the results for full detail:
     description, item specifics, return terms, and shipping.

Notes:
  - `min_price`/`max_price` require `price_currency`, and the currency must be
    the marketplace's own (USD for EBAY_US, GBP for EBAY_GB, and so on).
  - Prices come back as strings to preserve exact decimal values.
  - Results carry two identifiers. `itemId` (`v1|254188828753|0`) is what the
    item tools take; `legacyItemId` is the number in an ebay.com URL, which
    `get_ebay_item_by_listing_id` accepts.
  - Page with `offset`, using the `nextOffset` from the previous response.
    eBay will not page beyond roughly 10,000 results, so narrow the search
    rather than paging deeply.
  - Listings are live commerce data: prices, availability, and auction state
    change continuously, and a search reflects only the moment it ran.";

#[derive(Clone)]
pub struct EbayBrowseServer {
    client: Arc<BrowseClient>,
    tool_router: ToolRouter<Self>,
}

impl EbayBrowseServer {
    /// Build a server around a freshly configured Browse client.
    ///
    /// # Errors
    ///
    /// Returns [`reqwest::Error`] if the HTTP stack cannot be initialised.
    pub fn new(config: &Config) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Arc::new(BrowseClient::new(config)?),
            tool_router: Self::tool_router(),
        })
    }
}

/// Look up one listing by its RESTful eBay item id.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct GetItemRequest {
    /// The `itemId` from a search result, e.g. `v1|254188828753|0`.
    pub item_id: String,
    /// Optional extra blocks of detail.
    #[serde(default)]
    pub field_groups: Vec<ItemFieldGroup>,
    /// Marketplace to look the item up on. Defaults to the server's own.
    pub marketplace: Option<Marketplace>,
}

/// Look up one listing by the numeric id in its ebay.com URL.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct GetItemByListingIdRequest {
    /// The numeric listing id, e.g. `254188828753`. This is the `legacyItemId`
    /// in search results, and the digits in an `ebay.com/itm/...` URL.
    pub listing_id: String,
    /// Optional extra blocks of detail. `Compact` is not available here.
    #[serde(default)]
    pub field_groups: Vec<ItemFieldGroup>,
    /// Marketplace to look the item up on. Defaults to the server's own.
    pub marketplace: Option<Marketplace>,
}

/// Fetch every variation of a multi-variation listing.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct GetVariationsRequest {
    /// The `primaryItemGroupId` reported by `get_ebay_item` for an item that
    /// belongs to a variation group.
    pub item_group_id: String,
    /// Marketplace to look the group up on. Defaults to the server's own.
    pub marketplace: Option<Marketplace>,
}

/// Find listings whose pictures resemble a supplied image.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ImageSearchRequest {
    /// The image, base64-encoded, without a `data:` URI prefix. JPEG, PNG, GIF,
    /// BMP, TIFF, or WEBP.
    pub image_base64: String,
    /// The same narrowing criteria `search_ebay` accepts. Keyword fields
    /// (`query`, `gtin`, `epid`) are ignored here, since the image is the query.
    #[serde(default)]
    pub criteria: SearchRequest,
}

#[tool_router]
impl EbayBrowseServer {
    /// Search eBay listings by keyword, category, price, condition, seller,
    /// location, and more.
    ///
    /// Returns a page of matching listings. Request refinement field groups to
    /// discover the category ids and aspect names needed to narrow a broad
    /// search.
    #[tool(
        name = "search_ebay",
        description = "Search live eBay listings by keywords, category, price range, \
                       condition, buying format, seller, and delivery constraints. \
                       Returns a page of matching listings, optionally with refinement \
                       facets (categories, brands, conditions) for narrowing the search."
    )]
    pub async fn search_ebay(
        &self,
        Parameters(request): Parameters<SearchRequest>,
    ) -> Result<Json<SearchResults>, String> {
        tracing::info!(query = ?request.query, "searching eBay");
        self.client
            .search(&request)
            .await
            .map(Json)
            .map_err(describe)
    }

    /// Retrieve full detail for one listing.
    #[tool(
        name = "get_ebay_item",
        description = "Fetch complete detail for one eBay listing by its RESTful item id \
                       (the `itemId` field of a search result, e.g. `v1|254188828753|0`): \
                       description, item specifics, shipping options, return terms, \
                       seller, and images."
    )]
    pub async fn get_ebay_item(
        &self,
        Parameters(request): Parameters<GetItemRequest>,
    ) -> Result<Json<Item>, String> {
        tracing::info!(item_id = %request.item_id, "fetching eBay item");
        self.client
            .get_item(&request.item_id, &request.field_groups, request.marketplace)
            .await
            .map(Json)
            .map_err(describe)
    }

    /// Retrieve full detail for one listing, given the id from its web URL.
    #[tool(
        name = "get_ebay_item_by_listing_id",
        description = "Fetch complete detail for one eBay listing by its numeric listing id \
                       — the digits in an ebay.com/itm/... URL, reported as `legacyItemId` \
                       in search results. Use this when the user supplies an eBay link or \
                       item number rather than a RESTful item id."
    )]
    pub async fn get_ebay_item_by_listing_id(
        &self,
        Parameters(request): Parameters<GetItemByListingIdRequest>,
    ) -> Result<Json<Item>, String> {
        tracing::info!(listing_id = %request.listing_id, "fetching eBay item by legacy id");
        self.client
            .get_item_by_legacy_id(
                &request.listing_id,
                &request.field_groups,
                request.marketplace,
            )
            .await
            .map(Json)
            .map_err(describe)
    }

    /// List every variation of a multi-variation listing.
    #[tool(
        name = "get_ebay_item_variations",
        description = "List every variation (size, colour, capacity, ...) of a \
                       multi-variation eBay listing, given the `primaryItemGroupId` \
                       reported by get_ebay_item. Use this to compare prices and \
                       availability across variants of the same listing."
    )]
    pub async fn get_ebay_item_variations(
        &self,
        Parameters(request): Parameters<GetVariationsRequest>,
    ) -> Result<Json<ItemGroup>, String> {
        tracing::info!(item_group_id = %request.item_group_id, "fetching eBay item group");
        self.client
            .get_items_by_item_group(&request.item_group_id, request.marketplace)
            .await
            .map(Json)
            .map_err(describe)
    }

    /// Find listings that look like a supplied image.
    #[tool(
        name = "search_ebay_by_image",
        description = "Find eBay listings whose photos resemble a supplied image, given \
                       the image base64-encoded. Accepts the same narrowing criteria as \
                       search_ebay (price, condition, category, delivery). Use this when \
                       the user has a picture of an item but no reliable name for it."
    )]
    pub async fn search_ebay_by_image(
        &self,
        Parameters(request): Parameters<ImageSearchRequest>,
    ) -> Result<Json<SearchResults>, String> {
        tracing::info!("searching eBay by image");
        self.client
            .search_by_image(&request.image_base64, &request.criteria)
            .await
            .map(Json)
            .map_err(describe)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for EbayBrowseServer {
    fn get_info(&self) -> ServerInfo {
        server_info()
    }
}

/// The server's advertised identity and capabilities.
///
/// Free-standing rather than inline in `get_info` because it depends on
/// nothing but constants, which keeps it — and the tool schemas — testable
/// without standing up an HTTP client.
#[must_use]
pub fn server_info() -> ServerInfo {
    ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        // Spelled out rather than `Implementation::from_build_env()`, whose
        // `env!` calls resolve against rmcp's own crate metadata and would
        // announce this server as "rmcp".
        .with_server_info(
            Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                .with_title("eBay Browse")
                .with_description("Search and inspect live eBay listings via the Buy Browse API."),
        )
        .with_instructions(INSTRUCTIONS)
}

/// Turn a client failure into something the calling model can act on.
///
/// These strings become the body of an `is_error` tool result rather than a
/// protocol-level error, so the model reads them and gets a chance to fix the
/// call itself. That makes the remedy, not just the diagnosis, worth stating.
fn describe(error: BrowseError) -> String {
    let hint = match &error {
        BrowseError::InvalidCriteria(_) | BrowseError::UnsupportedFieldGroup { .. } => {
            Some("Adjust the parameters and call the tool again.")
        }
        BrowseError::Auth(_) => Some(
            "This is a server-side credential problem; the eBay application keyset \
             (EBAY_CLIENT_ID / EBAY_CLIENT_SECRET) is missing, wrong, or lacks Browse \
             API access. Retrying will not help.",
        ),
        BrowseError::RateLimited => {
            Some("Wait before retrying, and prefer fewer, more specific searches.")
        }
        BrowseError::NotFound(_) => Some(
            "Confirm the identifier, and check it belongs to the marketplace being \
             searched. Listings that have ended are also unavailable.",
        ),
        BrowseError::Api { status: 400, .. } => Some(
            "eBay rejected the criteria. Check that aspect names match what a search \
             with `field_groups: [\"AspectRefinements\"]` returned, and that any \
             `price_currency` matches the marketplace.",
        ),
        BrowseError::Transport(_) | BrowseError::Unexpected { .. } | BrowseError::Api { .. } => {
            None
        }
    };

    tracing::warn!(%error, "eBay request failed");
    match hint {
        Some(hint) => format!("{error}\n\n{hint}"),
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "panicking is how a test reports failure"
    )]

    use super::*;

    // The router and the advertised info are built from constants, so the MCP
    // surface is exercised without a `reqwest::Client` — whose construction
    // needs a system CA store that a sealed build sandbox does not provide.
    fn tools() -> Vec<rmcp::model::Tool> {
        EbayBrowseServer::tool_router().list_all()
    }

    #[test]
    fn every_tool_is_registered_and_described() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();

        for expected in [
            "search_ebay",
            "get_ebay_item",
            "get_ebay_item_by_listing_id",
            "get_ebay_item_variations",
            "search_ebay_by_image",
        ] {
            assert!(
                names.contains(&expected),
                "missing tool {expected}; have {names:?}"
            );
        }
        assert_eq!(tools.len(), 5, "unexpected tool set: {names:?}");

        for tool in &tools {
            assert!(
                tool.description.as_ref().is_some_and(|d| d.len() > 40),
                "tool {} needs a description a model can act on",
                tool.name
            );
        }
    }

    #[test]
    fn search_schema_enumerates_criteria_rather_than_free_text() {
        let tools = tools();
        let search = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "search_ebay")
            .unwrap();
        let schema = serde_json::to_value(&search.input_schema).unwrap();
        let properties = schema.get("properties").unwrap();

        // The point of the filter layer: a model picks typed fields, and never
        // has to hand-write eBay's `filter` DSL.
        for field in [
            "query",
            "min_price",
            "max_price",
            "conditions",
            "buying_options",
            "sort_by",
        ] {
            assert!(
                properties.get(field).is_some(),
                "missing `{field}` in search schema"
            );
        }

        // Enumerated criteria must reach the model as closed sets, not strings.
        let sort = schema
            .pointer("/$defs/SortOrder/oneOf")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert!(sort.iter().any(|variant| variant.get("const")
            == Some(&serde_json::Value::String("PriceLowest".to_owned()))));
    }

    #[test]
    fn server_info_advertises_tools_and_instructions() {
        let info = server_info();
        assert!(info.capabilities.tools.is_some());
        let instructions = info.instructions.unwrap();
        assert!(instructions.contains("price_currency"));
    }

    #[test]
    fn server_identifies_itself_and_not_the_sdk() {
        let info = server_info();
        assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }
}
