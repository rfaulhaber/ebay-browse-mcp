//! Typed search criteria, and their rendering into eBay's `filter` mini-language.
//!
//! The Browse API packs most search criteria into a single `filter` query
//! parameter with a syntax of its own: comma-separated `name:value` pairs,
//! where a value may be a scalar, a range `[low..high]` (either bound
//! omittable), or a set `{A|B|C}`. Getting a brace or a dot count wrong yields
//! an opaque HTTP 400.
//!
//! Rather than expose that string to callers, this module models each
//! criterion as a typed field and renders the DSL here, once, correctly.

use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Marketplace;

#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    #[error(
        "`min_price`/`max_price` require `price_currency` (an ISO 4217 code such \
         as USD, GBP, EUR) because eBay rejects a price filter without one"
    )]
    PriceWithoutCurrency,
    #[error("`min_price` ({min}) is greater than `max_price` ({max})")]
    InvertedPriceRange { min: f64, max: f64 },
    #[error("`{field}` must not be negative (got {value})")]
    NegativeValue { field: &'static str, value: f64 },
    #[error("`limit` must be between 1 and {max} (got {value})")]
    LimitOutOfRange { value: u32, max: u32 },
    #[error(
        "`offset` must be between 0 and {max} (got {value}); eBay caps the \
         reachable result window, so narrow the query instead of paging deeper"
    )]
    OffsetOutOfRange { value: u32, max: u32 },
    #[error(
        "`offset` ({offset}) must be zero or a whole multiple of `limit` ({limit}) \
         — eBay rejects any other pairing. The nearest valid offset is {nearest}"
    )]
    MisalignedOffset {
        offset: u32,
        limit: u32,
        nearest: u32,
    },
    #[error("`{field}` is not a usable date: {reason}. Use `YYYY-MM-DD` or an RFC 3339 timestamp")]
    BadDate { field: &'static str, reason: String },
    #[error(
        "a search needs at least one of `query`, `category_ids`, `gtin`, `epid`, \
         or `charity_ids`"
    )]
    NothingToSearchFor,
    #[error("`sort_by: Distance` also requires `pickup_country` and `pickup_postal_code`")]
    DistanceSortWithoutPickup,
}

/// eBay's hard ceiling on `limit` for `item_summary/search`.
pub const MAX_LIMIT: u32 = 200;
/// The page size eBay applies when `limit` is omitted.
pub const DEFAULT_LIMIT: u32 = 50;
/// eBay refuses to page past this offset; the reachable result window is 10,000
/// items, so the last addressable one sits at 9,999.
pub const MAX_OFFSET: u32 = 9_999;

/// How results are ordered. Omitting this leaves eBay's relevance ranking
/// ("Best Match") in place, which is usually what a shopper wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SortOrder {
    /// eBay's default relevance ranking.
    BestMatch,
    /// Cheapest first, by item price plus shipping.
    PriceLowest,
    /// Most expensive first, by item price plus shipping.
    PriceHighest,
    /// Most recently listed first.
    NewlyListed,
    /// Auctions and listings closing soonest first.
    EndingSoonest,
    /// Nearest first. Requires `pickup_country` and `pickup_postal_code`.
    Distance,
}

impl SortOrder {
    /// `None` means "send no `sort` parameter", which is how Best Match is
    /// requested. A leading `-` is eBay's descending marker.
    const fn as_query_value(self) -> Option<&'static str> {
        match self {
            Self::BestMatch => None,
            Self::PriceLowest => Some("price"),
            Self::PriceHighest => Some("-price"),
            Self::NewlyListed => Some("newlyListed"),
            Self::EndingSoonest => Some("endingSoonest"),
            Self::Distance => Some("distance"),
        }
    }
}

/// Item condition, as accepted by the `conditions` filter.
///
/// Deliberately only three values: eBay's `conditions` filter accepts exactly
/// `NEW`, `USED`, and `UNSPECIFIED`. Finer distinctions — open box, the four
/// refurbished tiers, for-parts — are expressed as numeric `condition_ids`,
/// not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Condition {
    /// Brand new, unopened.
    New,
    /// Previously owned, including refurbished grades.
    Used,
    /// The seller did not state a condition.
    Unspecified,
}

impl Condition {
    const fn as_filter_value(self) -> &'static str {
        match self {
            Self::New => "NEW",
            Self::Used => "USED",
            Self::Unspecified => "UNSPECIFIED",
        }
    }
}

/// How an item may be purchased.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BuyingOption {
    /// "Buy It Now" — a fixed price, immediately purchasable.
    FixedPrice,
    /// Bidding, with a close time.
    Auction,
    /// Seller accepts offers below the asking price.
    BestOffer,
    /// Classified ad; the transaction happens off eBay.
    ClassifiedAd,
}

impl BuyingOption {
    const fn as_filter_value(self) -> &'static str {
        match self {
            Self::FixedPrice => "FIXED_PRICE",
            Self::Auction => "AUCTION",
            Self::BestOffer => "BEST_OFFER",
            Self::ClassifiedAd => "CLASSIFIED_AD",
        }
    }
}

/// eBay buyer-protection and seller-quality programmes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum QualifiedProgram {
    /// Fast free delivery and free returns for eBay Plus members.
    EbayPlus,
    /// Item is authenticated by eBay before reaching the buyer.
    AuthenticityGuarantee,
    /// Item has passed eBay's authenticity verification.
    AuthenticityVerification,
}

impl QualifiedProgram {
    const fn as_filter_value(self) -> &'static str {
        match self {
            Self::EbayPlus => "EBAY_PLUS",
            Self::AuthenticityGuarantee => "AUTHENTICITY_GUARANTEE",
            Self::AuthenticityVerification => "AUTHENTICITY_VERIFICATION",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SellerAccountType {
    Business,
    Individual,
}

impl SellerAccountType {
    const fn as_filter_value(self) -> &'static str {
        match self {
            Self::Business => "BUSINESS",
            Self::Individual => "INDIVIDUAL",
        }
    }
}

/// Optional blocks of extra data in the search response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SearchFieldGroup {
    /// Aspect refinements (brand, size, colour...) with result counts — useful
    /// for discovering how to narrow a broad query.
    AspectRefinements,
    /// Counts per buying option.
    BuyingOptionRefinements,
    /// Counts per category, including the category ids to filter on next.
    CategoryRefinements,
    /// Counts per condition.
    ConditionRefinements,
    /// Adds `shortDescription` and item-level location data to each result.
    Extended,
    /// The matching listings themselves. Requesting any refinement group
    /// without this one returns facets and no items, so it is added
    /// automatically and never needs to be passed.
    MatchingItems,
    /// Every refinement group, plus the matching listings.
    Full,
}

impl SearchFieldGroup {
    const fn as_query_value(self) -> &'static str {
        match self {
            Self::AspectRefinements => "ASPECT_REFINEMENTS",
            Self::BuyingOptionRefinements => "BUYING_OPTION_REFINEMENTS",
            Self::CategoryRefinements => "CATEGORY_REFINEMENTS",
            Self::ConditionRefinements => "CONDITION_REFINEMENTS",
            Self::Extended => "EXTENDED",
            Self::MatchingItems => "MATCHING_ITEMS",
            Self::Full => "FULL",
        }
    }
}

/// Extra detail available from the item endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ItemFieldGroup {
    /// Adds the catalogue product behind the listing: `description`,
    /// `additionalImages`, aspect groups, and further product identifiers.
    Product,
    /// Trims the response to the few fields that change over a listing's life
    /// — price, availability, revision, end date. For re-checking a listing
    /// already seen, not for discovering one. Not available when looking an
    /// item up by its numeric listing id.
    Compact,
    /// Adds the seller's `userId`.
    AdditionalSellerDetails,
    /// Adds `charityTerms`, where the listing benefits a charity.
    CharityDetails,
}

impl ItemFieldGroup {
    pub(crate) const fn as_query_value(self) -> &'static str {
        match self {
            Self::Product => "PRODUCT",
            Self::Compact => "COMPACT",
            Self::AdditionalSellerDetails => "ADDITIONAL_SELLER_DETAILS",
            Self::CharityDetails => "CHARITY_DETAILS",
        }
    }

    /// Render a set of groups as the comma-separated `fieldgroups` value.
    #[must_use]
    pub fn join(groups: &[Self]) -> String {
        groups
            .iter()
            .map(|group| group.as_query_value())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Accumulates `name:value` clauses and joins them with the commas eBay expects.
#[derive(Debug, Default)]
struct FilterBuilder {
    clauses: Vec<String>,
}

impl FilterBuilder {
    fn scalar(&mut self, field: &str, value: impl std::fmt::Display) {
        self.clauses.push(format!("{field}:{value}"));
    }

    /// `field:{A|B|C}` — eBay's disjunctive set syntax.
    fn set<T>(&mut self, field: &str, values: &[T], render: impl Fn(&T) -> String) {
        if values.is_empty() {
            return;
        }
        let mut clause = format!("{field}:{{");
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                clause.push('|');
            }
            clause.push_str(&render(value));
        }
        clause.push('}');
        self.clauses.push(clause);
    }

    /// `field:[low..high]`, with either bound optional. Emits nothing when both
    /// are absent, so an unconstrained range never reaches eBay.
    fn range(&mut self, field: &str, low: Option<String>, high: Option<String>) {
        if low.is_none() && high.is_none() {
            return;
        }
        let mut clause = format!("{field}:[");
        if let Some(low) = low {
            clause.push_str(&low);
        }
        clause.push_str("..");
        if let Some(high) = high {
            clause.push_str(&high);
        }
        clause.push(']');
        self.clauses.push(clause);
    }

    fn finish(self) -> Option<String> {
        if self.clauses.is_empty() {
            None
        } else {
            Some(self.clauses.join(","))
        }
    }
}

/// A complete `item_summary/search` request.
///
/// Every field is optional, but a search must constrain *something*: eBay
/// rejects a request that supplies neither keywords nor a category nor a
/// product identifier.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SearchRequest {
    /// Keywords, as a shopper would type them. Multiple words are ANDed.
    /// Wrap a phrase in double quotes for an exact match, and prefix a word
    /// with `-` to exclude it (e.g. `iphone -case`). Max 100 characters.
    pub query: Option<String>,

    /// Restrict to these eBay category ids. Obtain them from a first search
    /// with `field_groups: ["CategoryRefinements"]`.
    #[serde(default)]
    pub category_ids: Vec<String>,

    /// Exclude these category ids from the results.
    #[serde(default)]
    pub exclude_category_ids: Vec<String>,

    /// Search by product barcode: UPC, EAN, or ISBN.
    pub gtin: Option<String>,

    /// eBay product identifier (ePID), which pins the search to one catalogue
    /// product across all its listings.
    pub epid: Option<String>,

    /// Restrict to listings benefiting these charity ids.
    #[serde(default)]
    pub charity_ids: Vec<String>,

    /// Narrow by item aspects such as brand or size. Obtain the exact aspect
    /// names and values from a first search with
    /// `field_groups: ["AspectRefinements"]` — they are category-specific and
    /// must match eBay's spelling exactly.
    #[serde(default)]
    pub aspects: Vec<AspectFilter>,

    /// Also match keywords appearing in listing descriptions, not just titles.
    /// Requires `query`, and broadens results considerably.
    pub search_in_description: Option<bool>,

    /// Let eBay correct apparent typos in `query`.
    #[serde(default = "default_true")]
    pub auto_correct: bool,

    // --- price -------------------------------------------------------------
    /// Lowest acceptable price, in `price_currency`. Requires `price_currency`.
    pub min_price: Option<f64>,
    /// Highest acceptable price, in `price_currency`. Requires `price_currency`.
    pub max_price: Option<f64>,
    /// ISO 4217 code the price bounds are expressed in (`USD`, `GBP`, `EUR`...).
    /// Must match the marketplace's currency.
    pub price_currency: Option<String>,

    // --- condition and buying ----------------------------------------------
    /// Acceptable item conditions. Omit to accept any.
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// Numeric eBay condition ids, the only way to express a distinction
    /// finer than `conditions`. Common ids: `1000` new, `1500` open box,
    /// `2000`/`2010`/`2020`/`2030` refurbished (certified through good),
    /// `2500` seller refurbished, `3000` used, `7000` for parts or not
    /// working. Valid ids vary by category.
    #[serde(default)]
    pub condition_ids: Vec<String>,
    /// Acceptable purchase mechanisms. Omit to accept any.
    #[serde(default)]
    pub buying_options: Vec<BuyingOption>,
    /// Only listings with this many bids, as `[min, max]`. Auctions only.
    pub bid_count: Option<[u32; 2]>,

    // --- location and delivery ---------------------------------------------
    /// Two-letter ISO 3166 code of the country the item ships *from*.
    pub item_location_country: Option<String>,
    /// Two-letter ISO 3166 code of the country the item must ship *to*.
    pub delivery_country: Option<String>,
    /// Buyer's postal code, which sharpens shipping costs and delivery dates.
    pub delivery_postal_code: Option<String>,
    /// Only listings that ship free.
    pub free_shipping_only: Option<bool>,
    /// Only listings guaranteed to arrive within this many days (1-4).
    /// Deprecated by eBay and increasingly ignored; prefer judging the
    /// `shippingOptions` delivery estimates on the results.
    pub max_delivery_days: Option<u32>,
    /// Country for a local-pickup radius search. Required by `sort_by: Distance`.
    pub pickup_country: Option<String>,
    /// Postal code for a local-pickup radius search. Required by
    /// `sort_by: Distance`.
    pub pickup_postal_code: Option<String>,
    /// Radius around `pickup_postal_code` to search within.
    pub pickup_radius: Option<u32>,
    /// Unit for `pickup_radius`.
    pub pickup_radius_unit: Option<DistanceUnit>,

    // --- seller and programmes ---------------------------------------------
    /// Only these sellers, by eBay username.
    #[serde(default)]
    pub sellers: Vec<String>,
    /// Everything except these sellers, by eBay username.
    #[serde(default)]
    pub exclude_sellers: Vec<String>,
    /// Restrict to business or private sellers.
    #[serde(default)]
    pub seller_account_types: Vec<SellerAccountType>,
    /// Only listings enrolled in these eBay programmes.
    #[serde(default)]
    pub qualified_programs: Vec<QualifiedProgram>,
    /// Only listings whose seller accepts returns.
    pub returns_accepted: Option<bool>,
    /// Only listings benefiting a charity.
    pub charity_only: Option<bool>,

    // --- listing dates ------------------------------------------------------
    /// Only listings created on or after this date. `YYYY-MM-DD` or RFC 3339.
    pub listed_after: Option<String>,
    /// Only listings created on or before this date.
    pub listed_before: Option<String>,
    /// Only listings ending on or after this date — i.e. still open then.
    pub ending_after: Option<String>,
    /// Only listings ending on or before this date. Useful with
    /// `sort_by: EndingSoonest` to find auctions closing within a window.
    pub ending_before: Option<String>,

    // --- result shaping ------------------------------------------------------
    /// Result ordering. Defaults to eBay's Best Match relevance ranking.
    pub sort_by: Option<SortOrder>,
    /// Results per page, 1-200. Defaults to 50.
    pub limit: Option<u32>,
    /// How many results to skip, for paging. Use the `next_offset` from a
    /// previous response.
    pub offset: Option<u32>,
    /// Extra response blocks. `CategoryRefinements` and `AspectRefinements` are
    /// the most useful, because they reveal how to narrow a broad search.
    #[serde(default)]
    pub field_groups: Vec<SearchFieldGroup>,
    /// Marketplace to search. Defaults to the server's configured marketplace.
    pub marketplace: Option<Marketplace>,

    /// Escape hatch: additional raw clauses in eBay's `filter` syntax, appended
    /// verbatim. Only needed for criteria this tool does not model.
    pub raw_filter: Option<String>,
}

const fn default_true() -> bool {
    true
}

/// One aspect constraint, e.g. Brand = Apple.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AspectFilter {
    /// Aspect name exactly as eBay spells it, e.g. `Brand`, `Storage Capacity`.
    pub name: String,
    /// Accepted values for the aspect; any one of them matches.
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DistanceUnit {
    Miles,
    Kilometers,
}

impl DistanceUnit {
    // eBay documents `mi` by example and does not enumerate the alternatives;
    // `km` is accepted on metric marketplaces.
    const fn as_filter_value(self) -> &'static str {
        match self {
            Self::Miles => "mi",
            Self::Kilometers => "km",
        }
    }
}

impl SearchRequest {
    /// Render this request as `(name, value)` query pairs for
    /// `item_summary/search`.
    ///
    /// Values are returned unencoded; the HTTP layer percent-encodes them.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] when the criteria are self-contradictory or
    /// violate a constraint eBay would reject with an opaque 400.
    pub fn to_query_pairs(&self) -> Result<Vec<(&'static str, String)>, FilterError> {
        self.validate()?;
        self.render_query(true)
    }

    /// Render the criteria for `item_summary/search_by_image`.
    ///
    /// The image itself is the query, so keyword and product-identifier fields
    /// are dropped and the "search for something" requirement does not apply.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] when the criteria are self-contradictory.
    pub fn to_image_query_pairs(&self) -> Result<Vec<(&'static str, String)>, FilterError> {
        self.validate_constraints()?;
        self.render_query(false)
    }

    fn render_query(
        &self,
        include_keywords: bool,
    ) -> Result<Vec<(&'static str, String)>, FilterError> {
        let mut pairs: Vec<(&'static str, String)> = Vec::new();

        if include_keywords {
            if let Some(query) = non_empty(self.query.as_deref()) {
                pairs.push(("q", query.to_owned()));
            }
            if let Some(gtin) = non_empty(self.gtin.as_deref()) {
                pairs.push(("gtin", gtin.to_owned()));
            }
            if let Some(epid) = non_empty(self.epid.as_deref()) {
                pairs.push(("epid", epid.to_owned()));
            }
            if self.auto_correct {
                // `KEYWORD` is the only value eBay accepts; omitting the
                // parameter is how correction is switched off.
                pairs.push(("auto_correct", "KEYWORD".to_owned()));
            }
        }

        if !self.category_ids.is_empty() {
            pairs.push(("category_ids", self.category_ids.join(",")));
        }
        if !self.charity_ids.is_empty() {
            pairs.push(("charity_ids", self.charity_ids.join(",")));
        }
        if let Some(aspect_filter) = self.render_aspect_filter() {
            pairs.push(("aspect_filter", aspect_filter));
        }
        if let Some(sort) = self.sort_by.and_then(SortOrder::as_query_value) {
            pairs.push(("sort", sort.to_owned()));
        }
        if let Some(limit) = self.limit {
            pairs.push(("limit", limit.to_string()));
        }
        if let Some(offset) = self.offset {
            pairs.push(("offset", offset.to_string()));
        }
        if !self.field_groups.is_empty() {
            let mut groups = self
                .field_groups
                .iter()
                .map(|group| group.as_query_value())
                .collect::<Vec<_>>();
            // eBay treats MATCHING_ITEMS as opt-in: ask only for refinements
            // and the response carries facets with no listings at all. Nobody
            // wants that, so supply it whenever it is missing.
            if !groups.contains(&"FULL") && !groups.contains(&"MATCHING_ITEMS") {
                groups.push("MATCHING_ITEMS");
            }
            pairs.push(("fieldgroups", groups.join(",")));
        }
        if let Some(filter) = self.render_filter()? {
            pairs.push(("filter", filter));
        }

        Ok(pairs)
    }

    fn validate(&self) -> Result<(), FilterError> {
        let has_target = non_empty(self.query.as_deref()).is_some()
            || non_empty(self.gtin.as_deref()).is_some()
            || non_empty(self.epid.as_deref()).is_some()
            || !self.category_ids.is_empty()
            || !self.charity_ids.is_empty();
        if !has_target {
            return Err(FilterError::NothingToSearchFor);
        }
        self.validate_constraints()
    }

    /// The rules that hold however the search was anchored.
    fn validate_constraints(&self) -> Result<(), FilterError> {
        if (self.min_price.is_some() || self.max_price.is_some())
            && non_empty(self.price_currency.as_deref()).is_none()
        {
            return Err(FilterError::PriceWithoutCurrency);
        }
        for (field, value) in [("min_price", self.min_price), ("max_price", self.max_price)] {
            if let Some(value) = value
                && value < 0.0
            {
                return Err(FilterError::NegativeValue { field, value });
            }
        }
        if let (Some(min), Some(max)) = (self.min_price, self.max_price)
            && min > max
        {
            return Err(FilterError::InvertedPriceRange { min, max });
        }

        if let Some(limit) = self.limit
            && (limit == 0 || limit > MAX_LIMIT)
        {
            return Err(FilterError::LimitOutOfRange {
                value: limit,
                max: MAX_LIMIT,
            });
        }
        if let Some(offset) = self.offset {
            if offset > MAX_OFFSET {
                return Err(FilterError::OffsetOutOfRange {
                    value: offset,
                    max: MAX_OFFSET,
                });
            }
            // eBay only accepts page boundaries: an offset that is not a whole
            // number of pages from the start is an error, not a shifted window.
            let limit = self.limit.unwrap_or(DEFAULT_LIMIT);
            if offset % limit != 0 {
                return Err(FilterError::MisalignedOffset {
                    offset,
                    limit,
                    nearest: (offset / limit) * limit,
                });
            }
        }

        if self.sort_by == Some(SortOrder::Distance)
            && (non_empty(self.pickup_country.as_deref()).is_none()
                || non_empty(self.pickup_postal_code.as_deref()).is_none())
        {
            return Err(FilterError::DistanceSortWithoutPickup);
        }

        Ok(())
    }

    /// `aspect_filter=categoryId:15032,Brand:{Apple|Samsung}`.
    ///
    /// eBay requires the category id to lead the clause, because aspect names
    /// are only meaningful within a category.
    fn render_aspect_filter(&self) -> Option<String> {
        let category = self.category_ids.first()?;
        if self.aspects.is_empty() {
            return None;
        }
        let mut rendered = format!("categoryId:{category}");
        for aspect in &self.aspects {
            if aspect.values.is_empty() {
                continue;
            }
            let values = aspect.values.join("|");
            // `write!` to a String cannot fail; the result is discarded rather
            // than unwrapped to keep the crate panic-free.
            let _ = write!(rendered, ",{}:{{{}}}", aspect.name, values);
        }
        Some(rendered)
    }

    fn render_filter(&self) -> Result<Option<String>, FilterError> {
        let mut builder = FilterBuilder::default();

        if self.min_price.is_some() || self.max_price.is_some() {
            builder.range(
                "price",
                self.min_price.map(format_money),
                self.max_price.map(format_money),
            );
            if let Some(currency) = non_empty(self.price_currency.as_deref()) {
                builder.scalar("priceCurrency", currency.to_ascii_uppercase());
            }
        }

        builder.set("conditions", &self.conditions, |c| {
            c.as_filter_value().to_owned()
        });
        builder.set("conditionIds", &self.condition_ids, Clone::clone);
        builder.set("buyingOptions", &self.buying_options, |option| {
            option.as_filter_value().to_owned()
        });
        builder.set("sellers", &self.sellers, Clone::clone);
        builder.set("excludeSellers", &self.exclude_sellers, Clone::clone);
        builder.set("sellerAccountTypes", &self.seller_account_types, |kind| {
            kind.as_filter_value().to_owned()
        });
        builder.set("qualifiedPrograms", &self.qualified_programs, |program| {
            program.as_filter_value().to_owned()
        });
        builder.set(
            "excludeCategoryIds",
            &self.exclude_category_ids,
            Clone::clone,
        );

        if let Some([min, max]) = self.bid_count {
            builder.range("bidCount", Some(min.to_string()), Some(max.to_string()));
        }

        if let Some(country) = non_empty(self.item_location_country.as_deref()) {
            builder.scalar("itemLocationCountry", country.to_ascii_uppercase());
        }
        if let Some(country) = non_empty(self.delivery_country.as_deref()) {
            builder.scalar("deliveryCountry", country.to_ascii_uppercase());
        }
        if let Some(postal) = non_empty(self.delivery_postal_code.as_deref()) {
            builder.scalar("deliveryPostalCode", postal);
        }
        if self.free_shipping_only == Some(true) {
            // eBay expresses "free shipping" as a delivery cost ceiling of zero.
            builder.scalar("maxDeliveryCost", 0);
        }
        if let Some(days) = self.max_delivery_days {
            builder.scalar("guaranteedDeliveryInDays", days);
        }
        if let Some(country) = non_empty(self.pickup_country.as_deref()) {
            builder.scalar("pickupCountry", country.to_ascii_uppercase());
        }
        if let Some(postal) = non_empty(self.pickup_postal_code.as_deref()) {
            builder.scalar("pickupPostalCode", postal);
        }
        if let Some(radius) = self.pickup_radius {
            builder.scalar("pickupRadius", radius);
            builder.scalar(
                "pickupRadiusUnit",
                self.pickup_radius_unit
                    .unwrap_or(DistanceUnit::Miles)
                    .as_filter_value(),
            );
        }

        if let Some(accepted) = self.returns_accepted {
            builder.scalar("returnsAccepted", accepted);
        }
        if let Some(charity_only) = self.charity_only {
            builder.scalar("charityOnly", charity_only);
        }
        if self.search_in_description == Some(true) {
            builder.scalar("searchInDescription", true);
        }

        let listed_after = parse_date("listed_after", self.listed_after.as_deref())?;
        let listed_before = parse_date("listed_before", self.listed_before.as_deref())?;
        builder.range("itemStartDate", listed_after, listed_before);

        let ending_after = parse_date("ending_after", self.ending_after.as_deref())?;
        let ending_before = parse_date("ending_before", self.ending_before.as_deref())?;
        builder.range("itemEndDate", ending_after, ending_before);

        let mut rendered = builder.finish();
        if let Some(raw) = non_empty(self.raw_filter.as_deref()) {
            rendered = Some(match rendered {
                Some(existing) => format!("{existing},{raw}"),
                None => raw.to_owned(),
            });
        }
        Ok(rendered)
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// eBay compares prices as decimal strings; `{:.2}` avoids scientific notation
/// and float artefacts like `19.989999999999998` reaching the wire.
fn format_money(value: f64) -> String {
    format!("{value:.2}")
}

/// Normalise a caller-supplied date into the `YYYY-MM-DDTHH:MM:SSZ` form eBay's
/// date-range filters document, accepting a bare date as midnight UTC.
fn parse_date(field: &'static str, value: Option<&str>) -> Result<Option<String>, FilterError> {
    let Some(value) = non_empty(value) else {
        return Ok(None);
    };

    let timestamp = value
        .parse::<jiff::Timestamp>()
        .or_else(|_| {
            value
                .parse::<jiff::civil::Date>()
                .map_err(|err| err.to_string())
                .and_then(|date| {
                    date.to_zoned(jiff::tz::TimeZone::UTC)
                        .map(|zoned| zoned.timestamp())
                        .map_err(|err| err.to_string())
                })
        })
        .map_err(|err| FilterError::BadDate {
            field,
            reason: err.to_string(),
        })?;

    Ok(Some(timestamp.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "panicking is how a test reports failure"
    )]

    use super::*;

    fn keyword_search(query: &str) -> SearchRequest {
        SearchRequest {
            query: Some(query.to_owned()),
            ..SearchRequest::default()
        }
    }

    fn rendered_filter(request: &SearchRequest) -> String {
        request
            .to_query_pairs()
            .unwrap()
            .into_iter()
            .find(|(name, _)| *name == "filter")
            .map(|(_, value)| value)
            .unwrap_or_default()
    }

    #[test]
    fn a_bare_keyword_search_sends_no_filter() {
        let pairs = keyword_search("cordless drill").to_query_pairs().unwrap();
        assert!(pairs.contains(&("q", "cordless drill".to_owned())));
        assert!(!pairs.iter().any(|(name, _)| *name == "filter"));
    }

    #[test]
    fn price_range_renders_ebays_bracket_syntax_with_currency() {
        let request = SearchRequest {
            min_price: Some(10.0),
            max_price: Some(49.5),
            price_currency: Some("usd".to_owned()),
            ..keyword_search("drill")
        };
        assert_eq!(
            rendered_filter(&request),
            "price:[10.00..49.50],priceCurrency:USD"
        );
    }

    #[test]
    fn an_open_ended_price_range_omits_the_missing_bound() {
        let request = SearchRequest {
            min_price: Some(100.0),
            price_currency: Some("USD".to_owned()),
            ..keyword_search("lens")
        };
        assert_eq!(
            rendered_filter(&request),
            "price:[100.00..],priceCurrency:USD"
        );

        let request = SearchRequest {
            max_price: Some(25.0),
            price_currency: Some("USD".to_owned()),
            ..keyword_search("lens")
        };
        assert_eq!(
            rendered_filter(&request),
            "price:[..25.00],priceCurrency:USD"
        );
    }

    #[test]
    fn price_without_currency_is_caught_before_ebay_sees_it() {
        let request = SearchRequest {
            max_price: Some(50.0),
            ..keyword_search("drill")
        };
        let err = request.to_query_pairs().unwrap_err();
        assert!(matches!(err, FilterError::PriceWithoutCurrency));
        // The message must tell the model what to do, not just what went wrong.
        assert!(err.to_string().contains("price_currency"));
    }

    #[test]
    fn an_inverted_price_range_is_rejected() {
        let request = SearchRequest {
            min_price: Some(90.0),
            max_price: Some(10.0),
            price_currency: Some("USD".to_owned()),
            ..keyword_search("drill")
        };
        assert!(matches!(
            request.to_query_pairs().unwrap_err(),
            FilterError::InvertedPriceRange { .. }
        ));
    }

    #[test]
    fn multi_valued_criteria_render_as_brace_sets() {
        let request = SearchRequest {
            conditions: vec![Condition::New, Condition::Used],
            buying_options: vec![BuyingOption::Auction, BuyingOption::FixedPrice],
            ..keyword_search("camera")
        };
        assert_eq!(
            rendered_filter(&request),
            "conditions:{NEW|USED},buyingOptions:{AUCTION|FIXED_PRICE}"
        );
    }

    #[test]
    fn free_shipping_becomes_a_zero_delivery_cost_ceiling() {
        let request = SearchRequest {
            free_shipping_only: Some(true),
            ..keyword_search("keyboard")
        };
        assert_eq!(rendered_filter(&request), "maxDeliveryCost:0");
    }

    #[test]
    fn country_codes_are_normalised_to_upper_case() {
        let request = SearchRequest {
            item_location_country: Some("de".to_owned()),
            delivery_country: Some("gb".to_owned()),
            ..keyword_search("bike")
        };
        assert_eq!(
            rendered_filter(&request),
            "itemLocationCountry:DE,deliveryCountry:GB"
        );
    }

    #[test]
    fn a_bare_date_becomes_midnight_utc_in_ebays_format() {
        let request = SearchRequest {
            listed_after: Some("2026-01-15".to_owned()),
            ..keyword_search("vinyl")
        };
        assert_eq!(
            rendered_filter(&request),
            "itemStartDate:[2026-01-15T00:00:00Z..]"
        );
    }

    #[test]
    fn an_rfc3339_timestamp_is_normalised_to_utc() {
        let request = SearchRequest {
            ending_before: Some("2026-03-01T12:30:00-05:00".to_owned()),
            ..keyword_search("vinyl")
        };
        assert_eq!(
            rendered_filter(&request),
            "itemEndDate:[..2026-03-01T17:30:00Z]"
        );
    }

    #[test]
    fn an_unparseable_date_names_the_offending_field() {
        let request = SearchRequest {
            listed_after: Some("last tuesday".to_owned()),
            ..keyword_search("vinyl")
        };
        let err = request.to_query_pairs().unwrap_err();
        assert!(err.to_string().contains("listed_after"), "{err}");
    }

    #[test]
    fn sort_order_maps_to_ebays_leading_minus_convention() {
        let pairs = SearchRequest {
            sort_by: Some(SortOrder::PriceHighest),
            ..keyword_search("watch")
        }
        .to_query_pairs()
        .unwrap();
        assert!(pairs.contains(&("sort", "-price".to_owned())));

        // Best Match is expressed by sending no `sort` at all.
        let pairs = SearchRequest {
            sort_by: Some(SortOrder::BestMatch),
            ..keyword_search("watch")
        }
        .to_query_pairs()
        .unwrap();
        assert!(!pairs.iter().any(|(name, _)| *name == "sort"));
    }

    #[test]
    fn distance_sort_requires_a_pickup_origin() {
        let request = SearchRequest {
            sort_by: Some(SortOrder::Distance),
            ..keyword_search("sofa")
        };
        assert!(matches!(
            request.to_query_pairs().unwrap_err(),
            FilterError::DistanceSortWithoutPickup
        ));
    }

    #[test]
    fn aspects_are_scoped_to_the_leading_category() {
        let request = SearchRequest {
            category_ids: vec!["9355".to_owned()],
            aspects: vec![AspectFilter {
                name: "Brand".to_owned(),
                values: vec!["Apple".to_owned(), "Samsung".to_owned()],
            }],
            ..keyword_search("phone")
        };
        let pairs = request.to_query_pairs().unwrap();
        assert!(pairs.contains(&(
            "aspect_filter",
            "categoryId:9355,Brand:{Apple|Samsung}".to_owned()
        )));
    }

    #[test]
    fn aspects_without_a_category_are_dropped_rather_than_sent_invalid() {
        // eBay requires a categoryId to lead the aspect_filter; sending aspects
        // alone is a guaranteed 400.
        let request = SearchRequest {
            aspects: vec![AspectFilter {
                name: "Brand".to_owned(),
                values: vec!["Apple".to_owned()],
            }],
            ..keyword_search("phone")
        };
        let pairs = request.to_query_pairs().unwrap();
        assert!(!pairs.iter().any(|(name, _)| *name == "aspect_filter"));
    }

    #[test]
    fn a_search_must_be_anchored_to_something() {
        let err = SearchRequest::default().to_query_pairs().unwrap_err();
        assert!(matches!(err, FilterError::NothingToSearchFor));

        // A category alone is a legitimate anchor.
        let request = SearchRequest {
            category_ids: vec!["9355".to_owned()],
            ..SearchRequest::default()
        };
        assert!(request.to_query_pairs().is_ok());
    }

    #[test]
    fn image_search_needs_no_anchor_and_drops_keywords() {
        let criteria = SearchRequest {
            query: Some("ignored".to_owned()),
            max_price: Some(30.0),
            price_currency: Some("USD".to_owned()),
            ..SearchRequest::default()
        };
        let pairs = criteria.to_image_query_pairs().unwrap();
        assert!(!pairs.iter().any(|(name, _)| *name == "q"));
        assert!(pairs.iter().any(|(name, _)| *name == "filter"));
    }

    #[test]
    fn limit_and_offset_are_bounded() {
        let over_limit = SearchRequest {
            limit: Some(MAX_LIMIT + 1),
            ..keyword_search("x")
        };
        assert!(matches!(
            over_limit.to_query_pairs().unwrap_err(),
            FilterError::LimitOutOfRange { .. }
        ));

        let zero_limit = SearchRequest {
            limit: Some(0),
            ..keyword_search("x")
        };
        assert!(matches!(
            zero_limit.to_query_pairs().unwrap_err(),
            FilterError::LimitOutOfRange { .. }
        ));

        let deep = SearchRequest {
            offset: Some(MAX_OFFSET + 1),
            ..keyword_search("x")
        };
        assert!(matches!(
            deep.to_query_pairs().unwrap_err(),
            FilterError::OffsetOutOfRange { .. }
        ));
    }

    #[test]
    fn refinement_requests_still_ask_for_the_listings_themselves() {
        // Left alone, eBay returns facets and no items for this request.
        let request = SearchRequest {
            field_groups: vec![
                SearchFieldGroup::CategoryRefinements,
                SearchFieldGroup::AspectRefinements,
            ],
            ..keyword_search("drill")
        };
        let pairs = request.to_query_pairs().unwrap();
        let groups = pairs
            .iter()
            .find(|(name, _)| *name == "fieldgroups")
            .map(|(_, value)| value.as_str())
            .unwrap();
        assert_eq!(
            groups,
            "CATEGORY_REFINEMENTS,ASPECT_REFINEMENTS,MATCHING_ITEMS"
        );
    }

    #[test]
    fn full_already_implies_the_listings_and_is_left_alone() {
        let request = SearchRequest {
            field_groups: vec![SearchFieldGroup::Full],
            ..keyword_search("drill")
        };
        let pairs = request.to_query_pairs().unwrap();
        assert!(pairs.contains(&("fieldgroups", "FULL".to_owned())));
    }

    #[test]
    fn matching_items_is_not_duplicated_when_asked_for_explicitly() {
        let request = SearchRequest {
            field_groups: vec![
                SearchFieldGroup::MatchingItems,
                SearchFieldGroup::ConditionRefinements,
            ],
            ..keyword_search("drill")
        };
        let pairs = request.to_query_pairs().unwrap();
        assert!(pairs.contains(&(
            "fieldgroups",
            "MATCHING_ITEMS,CONDITION_REFINEMENTS".to_owned()
        )));
    }

    #[test]
    fn image_search_keeps_charity_ids_which_ebay_accepts_there() {
        let criteria = SearchRequest {
            charity_ids: vec!["13-1788491".to_owned()],
            ..SearchRequest::default()
        };
        let pairs = criteria.to_image_query_pairs().unwrap();
        assert!(pairs.contains(&("charity_ids", "13-1788491".to_owned())));
        // ...but not auto_correct, which that endpoint does not take.
        assert!(!pairs.iter().any(|(name, _)| *name == "auto_correct"));
    }

    #[test]
    fn an_offset_that_is_not_a_page_boundary_is_rejected() {
        // eBay errors on these rather than shifting the window, so the message
        // names the offset the caller almost certainly meant.
        let request = SearchRequest {
            limit: Some(20),
            offset: Some(35),
            ..keyword_search("x")
        };
        let err = request.to_query_pairs().unwrap_err();
        assert!(matches!(
            err,
            FilterError::MisalignedOffset { nearest: 20, .. }
        ));
        assert!(err.to_string().contains("20"), "{err}");
    }

    #[test]
    fn page_aligned_offsets_pass_including_against_the_implicit_default_limit() {
        let with_limit = SearchRequest {
            limit: Some(20),
            offset: Some(40),
            ..keyword_search("x")
        };
        assert!(with_limit.to_query_pairs().is_ok());

        // No explicit limit: alignment is measured against eBay's default of 50.
        let implicit = SearchRequest {
            offset: Some(100),
            ..keyword_search("x")
        };
        assert!(implicit.to_query_pairs().is_ok());

        let misaligned = SearchRequest {
            offset: Some(60),
            ..keyword_search("x")
        };
        assert!(matches!(
            misaligned.to_query_pairs().unwrap_err(),
            FilterError::MisalignedOffset { limit: 50, .. }
        ));
    }

    #[test]
    fn a_raw_filter_is_appended_to_the_generated_clauses() {
        let request = SearchRequest {
            free_shipping_only: Some(true),
            raw_filter: Some("charityOnly:true".to_owned()),
            ..keyword_search("mug")
        };
        assert_eq!(
            rendered_filter(&request),
            "maxDeliveryCost:0,charityOnly:true"
        );
    }

    #[test]
    fn prices_never_reach_the_wire_in_scientific_notation() {
        assert_eq!(format_money(0.000_01), "0.00");
        assert_eq!(format_money(1e7), "10000000.00");
        assert_eq!(format_money(19.99), "19.99");
    }
}
