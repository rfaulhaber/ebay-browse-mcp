//! End-to-end checks against the real eBay Browse API.
//!
//! Ignored by default: they need a working application keyset in the
//! environment and consume the account's daily call allowance. Run with
//!
//! ```text
//! cargo test --test live -- --ignored --nocapture
//! ```
//!
//! These assert on the *shape* of eBay's answers, never on specific listings —
//! the catalogue is live and changes minute to minute.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "panicking is how a test reports failure"
)]

use ebay_browse_mcp::{
    Config,
    ebay::{
        BrowseClient,
        filter::{BuyingOption, Condition, SearchFieldGroup, SearchRequest, SortOrder},
    },
};

fn client() -> BrowseClient {
    let config = Config::from_env()
        .expect("set EBAY_CLIENT_ID and EBAY_CLIENT_SECRET to run the live tests");
    BrowseClient::new(&config).unwrap()
}

fn keyword(query: &str) -> SearchRequest {
    SearchRequest {
        query: Some(query.to_owned()),
        limit: Some(5),
        ..SearchRequest::default()
    }
}

#[tokio::test]
#[ignore = "hits the live eBay API"]
async fn a_keyword_search_returns_usable_listings() {
    let results = client().search(&keyword("cordless drill")).await.unwrap();

    assert!(results.total.unwrap_or(0) > 0, "expected some matches");
    assert!(!results.items.is_empty());

    for item in &results.items {
        // `itemId` is the handle every other tool takes; without it a result is
        // a dead end.
        assert!(
            item.item_id.starts_with("v1|"),
            "unexpected id {}",
            item.item_id
        );
        assert!(item.title.is_some());
    }
}

#[tokio::test]
#[ignore = "hits the live eBay API"]
async fn the_generated_filter_string_is_one_ebay_accepts() {
    // The whole filter DSL, exercised at once. A syntax slip anywhere here is
    // an HTTP 400 rather than a wrong answer, which is what makes this worth
    // running against the real API.
    let request = SearchRequest {
        min_price: Some(20.0),
        max_price: Some(500.0),
        price_currency: Some("USD".to_owned()),
        conditions: vec![Condition::New, Condition::Used],
        buying_options: vec![BuyingOption::FixedPrice],
        item_location_country: Some("US".to_owned()),
        free_shipping_only: Some(true),
        returns_accepted: Some(true),
        sort_by: Some(SortOrder::PriceLowest),
        ..keyword("laptop")
    };

    let results = client().search(&request).await.unwrap();
    assert!(
        results.warnings.is_empty(),
        "eBay complained: {:?}",
        results.warnings
    );

    // Ascending price sort must actually hold.
    let prices: Vec<f64> = results
        .items
        .iter()
        .filter_map(|item| item.price.as_ref())
        .filter_map(|money| money.value.parse::<f64>().ok())
        .collect();
    assert!(
        prices.windows(2).all(|pair| pair[0] <= pair[1]),
        "results are not price-ascending: {prices:?}"
    );
}

#[tokio::test]
#[ignore = "hits the live eBay API"]
async fn refinements_come_back_when_requested() {
    let request = SearchRequest {
        field_groups: vec![
            SearchFieldGroup::CategoryRefinements,
            SearchFieldGroup::AspectRefinements,
        ],
        ..keyword("running shoes")
    };
    let results = client().search(&request).await.unwrap();
    let refinements = results.refinements.expect("asked for refinements");
    assert!(
        !refinements.category_distributions.is_empty(),
        "expected category refinements to narrow with"
    );
}

#[tokio::test]
#[ignore = "hits the live eBay API"]
async fn a_search_hit_can_be_fetched_in_full() {
    let client = client();
    let results = client
        .search(&keyword("mechanical keyboard"))
        .await
        .unwrap();
    let first = results.items.first().expect("expected at least one result");

    let item = client.get_item(&first.item_id, &[], None).await.unwrap();
    assert_eq!(item.item_id, first.item_id);

    // The round trip search -> detail -> legacy-id lookup must be consistent,
    // since agents move between the two identifiers.
    if let Some(legacy) = &item.legacy_item_id {
        let by_legacy = client
            .get_item_by_legacy_id(legacy, &[], None)
            .await
            .unwrap();
        assert_eq!(by_legacy.item_id, item.item_id);
    }
}

#[tokio::test]
#[ignore = "hits the live eBay API"]
async fn paging_advances_through_distinct_results() {
    let client = client();
    let first = client.search(&keyword("headphones")).await.unwrap();
    let next_offset = first.next_offset.expect("expected more than one page");

    let second = client
        .search(&SearchRequest {
            offset: Some(next_offset),
            ..keyword("headphones")
        })
        .await
        .unwrap();

    let first_ids: Vec<&str> = first.items.iter().map(|i| i.item_id.as_str()).collect();
    assert!(
        second
            .items
            .iter()
            .all(|item| !first_ids.contains(&item.item_id.as_str())),
        "second page repeated results from the first"
    );
}

#[tokio::test]
#[ignore = "hits the live eBay API"]
async fn a_bogus_category_surfaces_ebays_own_complaint() {
    // An implausible category id does not fail the call: eBay documents 12009
    // as a *warning* — the search runs with the bad parameter dropped. What an
    // agent needs is the complaint riding along in `warnings`; without it the
    // response is indistinguishable from an unfiltered search that worked.
    let request = SearchRequest {
        category_ids: vec!["999999999".to_owned()],
        ..keyword("chair")
    };
    let results = client().search(&request).await.unwrap();
    assert!(
        results
            .warnings
            .iter()
            .any(|warning| warning.error_id == Some(12009)),
        "expected eBay's category_ids complaint among the warnings, got {:?}",
        results.warnings
    );
}
