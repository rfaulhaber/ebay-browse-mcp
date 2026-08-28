//! Response types for the Browse API.
//!
//! These are deliberately narrower than eBay's wire format. A raw
//! `itemSummaries` array carries a great deal that no shopping agent reads —
//! image variants, marketing price history, adult-content flags, per-locale
//! aspect blocks — and it is paid for in context tokens on every call. Serde
//! drops any field absent from these structs, so the struct definition *is*
//! the projection: what is written here is what an agent sees.
//!
//! Field names stay in eBay's camelCase so that anything an agent already
//! knows from eBay's own documentation continues to apply.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A monetary amount.
///
/// eBay sends the amount as a *string*, not a number, to avoid binary
/// floating-point rounding on currency values; it is kept that way here.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Money {
    /// Decimal amount, e.g. `"19.99"`.
    pub value: String,
    /// ISO 4217 currency code, e.g. `"USD"`.
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Seller {
    pub username: Option<String>,
    /// Positive-feedback percentage as a string, e.g. `"98.6"`.
    pub feedback_percentage: Option<String>,
    pub feedback_score: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub city: Option<String>,
    pub state_or_province: Option<String>,
    pub postal_code: Option<String>,
    /// Two-letter ISO 3166 country code.
    pub country: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub category_id: Option<String>,
    pub category_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShippingOption {
    /// `FIXED` or `CALCULATED`. A calculated cost depends on the buyer's
    /// address, so it is only meaningful when the request supplied one.
    pub shipping_cost_type: Option<String>,
    /// Absent for calculated shipping with no buyer address on the request.
    pub shipping_cost: Option<Money>,
    pub min_estimated_delivery_date: Option<String>,
    pub max_estimated_delivery_date: Option<String>,
    pub guaranteed_delivery: Option<bool>,
}

/// One search hit.
///
/// Note the two identifiers: `itemId` is the RESTful id (`v1|123456789|0`)
/// that the other Browse endpoints take, while `legacyItemId` is the plain
/// numeric id visible in an ebay.com URL.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemSummary {
    /// RESTful item id, e.g. `v1|254188828753|0`. Pass this to `get_ebay_item`.
    pub item_id: String,
    /// Numeric listing id as it appears in an ebay.com URL.
    pub legacy_item_id: Option<String>,
    pub title: Option<String>,
    /// Human-readable condition, e.g. `"New"`, `"Pre-owned"`.
    pub condition: Option<String>,
    pub condition_id: Option<String>,
    /// Asking price for fixed-price listings.
    pub price: Option<Money>,
    /// Present on auctions; the standing high bid.
    pub current_bid_price: Option<Money>,
    pub bid_count: Option<i64>,
    /// e.g. `["FIXED_PRICE", "BEST_OFFER"]`.
    #[serde(default)]
    pub buying_options: Vec<String>,
    #[serde(default)]
    pub shipping_options: Vec<ShippingOption>,
    pub seller: Option<Seller>,
    pub item_location: Option<Location>,
    pub image: Option<Image>,
    /// Human-facing listing page.
    pub item_web_url: Option<String>,
    /// Listing page carrying affiliate tracking. Only present when the server
    /// is configured with an eBay Partner Network campaign id.
    pub item_affiliate_web_url: Option<String>,
    /// When the listing was created (ISO 8601).
    pub item_creation_date: Option<String>,
    /// When the listing closes (ISO 8601). Absent for listings with no end date.
    pub item_end_date: Option<String>,
    pub top_rated_buying_experience: Option<bool>,
    #[serde(default)]
    pub categories: Vec<Category>,
    /// Only populated when the `Extended` field group is requested.
    pub short_description: Option<String>,
}

/// A refinement bucket: a value an agent can filter on next, with the number
/// of results that would remain.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CategoryDistribution {
    pub category_id: Option<String>,
    pub category_name: Option<String>,
    pub match_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValueDistribution {
    pub value: Option<String>,
    pub match_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AspectDistribution {
    /// Aspect name, e.g. `"Brand"`. Feed this back as `aspects[].name`.
    pub localized_aspect_name: Option<String>,
    #[serde(default)]
    pub aspect_value_distributions: Vec<ValueDistribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConditionDistribution {
    pub condition: Option<String>,
    pub condition_id: Option<String>,
    pub match_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BuyingOptionDistribution {
    pub buying_option: Option<String>,
    pub match_count: Option<i64>,
}

/// Ways to narrow the current search, returned when the corresponding field
/// group is requested. This is the intended path from a broad query to a
/// precise one.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Refinements {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub category_distributions: Vec<CategoryDistribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aspect_distributions: Vec<AspectDistribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub condition_distributions: Vec<ConditionDistribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buying_option_distributions: Vec<BuyingOptionDistribution>,
    pub dominant_category_id: Option<String>,
}

/// eBay's error and warning payload; both use the same shape.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub error_id: Option<i64>,
    pub domain: Option<String>,
    pub category: Option<String>,
    pub message: Option<String>,
    pub long_message: Option<String>,
    #[serde(default)]
    pub parameters: Vec<ErrorParameter>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorParameter {
    pub name: Option<String>,
    pub value: Option<String>,
}

impl ApiError {
    /// A one-line rendering suitable for an error message shown to an agent.
    #[must_use]
    pub fn summarize(&self) -> String {
        let text = self
            .long_message
            .as_deref()
            .or(self.message.as_deref())
            .unwrap_or("unspecified error");
        match self.error_id {
            Some(id) => format!("[{id}] {text}"),
            None => text.to_owned(),
        }
    }
}

/// What eBay silently changed about the request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutoCorrections {
    /// The spell-corrected keywords eBay actually searched for. Worth relaying
    /// to a user: it explains results that look off-target.
    pub q: Option<String>,
}

/// Envelope returned by `item_summary/search` and `item_summary/search_by_image`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSearchResponse {
    pub total: Option<i64>,
    pub auto_corrections: Option<AutoCorrections>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    /// Fully-formed URL for the next page, or absent on the last page.
    pub next: Option<String>,
    #[serde(default)]
    pub item_summaries: Vec<ItemSummary>,
    #[serde(default)]
    pub warnings: Vec<ApiError>,
    pub refinement: Option<Refinements>,
}

/// What a search tool hands back.
///
/// Paging is re-expressed as `nextOffset` rather than eBay's opaque `next`
/// URL, because the caller drives the next request through this tool's own
/// `offset` parameter and never fetches a URL directly.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    /// Total matches available, which may greatly exceed the returned page.
    pub total: Option<i64>,
    /// Page size actually applied.
    pub limit: Option<u32>,
    /// Offset of this page.
    pub offset: Option<u32>,
    /// Offset to request for the next page. Absent when this is the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
    /// Present when eBay spell-corrected the query rather than searching it
    /// verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_corrections: Option<AutoCorrections>,
    pub items: Vec<ItemSummary>,
    /// Present only when refinement field groups were requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refinements: Option<Refinements>,
    /// Non-fatal complaints from eBay — for instance an ignored filter. Worth
    /// surfacing, because a silently dropped filter looks like a wrong answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ApiError>,
}

impl From<RawSearchResponse> for SearchResults {
    fn from(raw: RawSearchResponse) -> Self {
        // eBay signals "more pages exist" with a `next` URL. Rather than parse
        // its offset out, derive the value: a next page exists exactly when
        // eBay says so, and its offset is this page's offset plus its size.
        let next_offset = raw.next.as_ref().and(
            raw.offset
                .zip(raw.limit)
                .map(|(offset, limit)| offset.saturating_add(limit)),
        );

        Self {
            total: raw.total,
            limit: raw.limit,
            offset: raw.offset,
            next_offset,
            auto_corrections: raw.auto_corrections,
            items: raw.item_summaries,
            refinements: raw.refinement,
            warnings: raw.warnings,
        }
    }
}

/// Full item detail from `item/{item_id}` and its siblings.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub item_id: String,
    pub legacy_item_id: Option<String>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    /// Seller-authored listing description. Raw HTML, and often very long —
    /// expect thousands of characters.
    pub description: Option<String>,
    /// Condensed condition and aspect summary. Much shorter than
    /// `description`, and usually enough to judge a listing.
    pub short_description: Option<String>,
    pub condition: Option<String>,
    pub condition_id: Option<String>,
    pub condition_description: Option<String>,
    pub price: Option<Money>,
    pub current_bid_price: Option<Money>,
    pub bid_count: Option<i64>,
    #[serde(default)]
    pub buying_options: Vec<String>,
    pub brand: Option<String>,
    pub mpn: Option<String>,
    pub gtin: Option<String>,
    pub epid: Option<String>,
    pub color: Option<String>,
    pub size: Option<String>,
    pub material: Option<String>,
    /// Category-specific attributes, e.g. Storage Capacity = 256 GB.
    #[serde(default)]
    pub localized_aspects: Vec<LocalizedAspect>,
    #[serde(default)]
    pub shipping_options: Vec<ShippingOption>,
    pub seller: Option<Seller>,
    pub item_location: Option<Location>,
    pub image: Option<Image>,
    #[serde(default)]
    pub additional_images: Vec<Image>,
    pub item_web_url: Option<String>,
    /// Listing page carrying affiliate tracking. Only present when the server
    /// is configured with an eBay Partner Network campaign id.
    pub item_affiliate_web_url: Option<String>,
    pub item_creation_date: Option<String>,
    pub item_end_date: Option<String>,
    pub estimated_availabilities: Option<serde_json::Value>,
    pub return_terms: Option<ReturnTerms>,
    #[serde(default)]
    pub ship_to_locations: Option<serde_json::Value>,
    pub category_path: Option<String>,
    pub category_id: Option<String>,
    /// Present when this item is one variation of a multi-variation listing.
    /// Its `itemGroupId` is what `get_ebay_item_variations` takes.
    pub primary_item_group: Option<ItemGroupSummary>,
    pub top_rated_buying_experience: Option<bool>,
    #[serde(default)]
    pub warnings: Vec<ApiError>,
}

/// Identifies the variation group a listing belongs to.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemGroupSummary {
    /// Pass this to `get_ebay_item_variations` to see every sibling variation.
    pub item_group_id: Option<String>,
    pub item_group_title: Option<String>,
    /// `SELLER_DEFINED_VARIATIONS` for a normal multi-variation listing.
    pub item_group_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LocalizedAspect {
    pub name: Option<String>,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReturnTerms {
    pub returns_accepted: Option<bool>,
    pub refund_method: Option<String>,
    pub return_shipping_cost_payer: Option<String>,
    pub return_period: Option<TimeDuration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeDuration {
    pub value: Option<i64>,
    /// e.g. `DAY`, `MONTH`.
    pub unit: Option<String>,
}

/// Response from `item/get_items_by_item_group`: every variation of a
/// multi-variation listing (sizes, colours, and so on).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemGroup {
    #[serde(default)]
    pub items: Vec<Item>,
    /// Shared media and aspect data for the group as a whole.
    pub common_descriptions: Option<serde_json::Value>,
    #[serde(default)]
    pub warnings: Vec<ApiError>,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "panicking is how a test reports failure"
    )]

    use super::*;

    #[test]
    fn search_response_projects_unknown_fields_away() {
        let raw = serde_json::json!({
            "href": "https://api.ebay.com/buy/browse/v1/item_summary/search?q=drill",
            "total": 1234,
            "limit": 50,
            "offset": 0,
            "next": "https://api.ebay.com/buy/browse/v1/item_summary/search?q=drill&offset=50",
            "itemSummaries": [{
                "itemId": "v1|254188828753|0",
                "legacyItemId": "254188828753",
                "title": "Cordless Drill",
                "price": { "value": "89.99", "currency": "USD" },
                "adultOnly": false,
                "thumbnailImages": [{ "imageUrl": "https://example.invalid/t.jpg" }],
                "image": { "imageUrl": "https://example.invalid/i.jpg" },
                "buyingOptions": ["FIXED_PRICE"],
                "seller": { "username": "toolshop", "feedbackPercentage": "99.2", "feedbackScore": 8421 }
            }]
        });

        let parsed: RawSearchResponse = serde_json::from_value(raw).unwrap();
        let results = SearchResults::from(parsed);

        assert_eq!(results.total, Some(1234));
        assert_eq!(results.next_offset, Some(50));
        assert_eq!(results.items.len(), 1);

        let item = &results.items[0];
        assert_eq!(item.item_id, "v1|254188828753|0");
        assert_eq!(item.price.as_ref().unwrap().value, "89.99");

        // `thumbnailImages` and `adultOnly` were in the input and must not
        // survive into what the agent pays tokens for.
        let out = serde_json::to_value(&results).unwrap();
        let serialized = out.to_string();
        assert!(!serialized.contains("thumbnailImages"));
        assert!(!serialized.contains("adultOnly"));
    }

    #[test]
    fn the_schema_never_requires_a_field_serialization_can_omit() {
        // MCP clients validate structured content against the declared output
        // schema, so a field that is `required` there but skipped when empty
        // fails every response that omits it — the happy path.
        let raw = serde_json::json!({
            "total": 0, "limit": 50, "offset": 0, "itemSummaries": []
        });
        let results =
            SearchResults::from(serde_json::from_value::<RawSearchResponse>(raw).unwrap());
        let serialized = serde_json::to_value(&results).unwrap();

        let schema = serde_json::to_value(schemars::schema_for!(SearchResults)).unwrap();
        for name in schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
        {
            assert!(
                serialized.get(name).is_some(),
                "schema requires `{name}` but a minimal response omits it"
            );
        }
    }

    #[test]
    fn last_page_has_no_next_offset() {
        let raw = serde_json::json!({
            "total": 12, "limit": 50, "offset": 0, "itemSummaries": []
        });
        let results =
            SearchResults::from(serde_json::from_value::<RawSearchResponse>(raw).unwrap());
        assert_eq!(results.next_offset, None);
    }

    #[test]
    fn a_spell_correction_is_surfaced_rather_than_swallowed() {
        // Without this, a search for "labtop" returns laptops and the agent has
        // no way to know the query was rewritten.
        let raw = serde_json::json!({
            "total": 5, "limit": 50, "offset": 0,
            "autoCorrections": { "q": "laptop" },
            "itemSummaries": []
        });
        let results =
            SearchResults::from(serde_json::from_value::<RawSearchResponse>(raw).unwrap());
        assert_eq!(
            results.auto_corrections.and_then(|correction| correction.q),
            Some("laptop".to_owned())
        );
    }

    #[test]
    fn a_variation_group_id_is_read_from_the_nested_object() {
        // eBay reports this as `primaryItemGroup.itemGroupId`, not as a
        // top-level id.
        let raw = serde_json::json!({
            "itemId": "v1|111|0",
            "primaryItemGroup": {
                "itemGroupId": "999888777",
                "itemGroupType": "SELLER_DEFINED_VARIATIONS"
            }
        });
        let item: Item = serde_json::from_value(raw).unwrap();
        let group = item.primary_item_group.unwrap();
        assert_eq!(group.item_group_id.as_deref(), Some("999888777"));
    }

    #[test]
    fn money_survives_as_a_string() {
        // A naive f64 mapping would both fail to parse and lose precision.
        let money: Money =
            serde_json::from_str(r#"{"value":"1234567.89","currency":"USD"}"#).unwrap();
        assert_eq!(money.value, "1234567.89");
    }

    #[test]
    fn api_error_summary_includes_the_id() {
        let err = ApiError {
            error_id: Some(12001),
            domain: None,
            category: None,
            message: Some("short".to_owned()),
            long_message: Some("The 'filter' value is invalid.".to_owned()),
            parameters: vec![],
        };
        assert_eq!(err.summarize(), "[12001] The 'filter' value is invalid.");
    }
}
