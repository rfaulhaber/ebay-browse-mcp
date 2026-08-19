# ebay-browse-mcp

An [MCP](https://modelcontextprotocol.io) server that lets an agent search and
inspect live eBay listings through eBay's
[Buy Browse API](https://developer.ebay.com/api-docs/buy/browse/overview.html).

Written in Rust on the official [`rmcp`](https://crates.io/crates/rmcp) SDK,
speaking MCP over stdio.

## What it exposes

| Tool | Purpose |
| --- | --- |
| `search_ebay` | Search listings by keywords, category, price, condition, buying format, seller, location, and delivery constraints. Optionally returns refinement facets. |
| `get_ebay_item` | Full detail for one listing, by its RESTful item id (`v1\|254188828753\|0`). |
| `get_ebay_item_by_listing_id` | Full detail for one listing, by the numeric id in an `ebay.com/itm/...` URL. |
| `get_ebay_item_variations` | Every variation (size, colour, capacity) of a multi-variation listing. |
| `search_ebay_by_image` | Listings whose photos resemble a supplied base64 image. |

That is the whole of the current Browse API's read surface, bar
`check_compatibility` (which answers "does this part fit this vehicle?" and
needs a caller who already knows the vehicle's aspects). The shopping-cart
methods that older documentation mentions are no longer part of the API.

### Search criteria are typed, not stringly

eBay packs most search criteria into one `filter` query parameter with a syntax
of its own — `price:[10..50],priceCurrency:USD,buyingOptions:{AUCTION|FIXED_PRICE}`.
Handing that to a language model reliably produces HTTP 400s.

This server exposes each criterion as a typed, enumerated tool parameter and
renders the DSL itself:

```jsonc
{
  "query": "cordless drill",
  "min_price": 40, "max_price": 120, "price_currency": "USD",
  "conditions": ["New", "CertifiedRefurbished"],
  "buying_options": ["FixedPrice"],
  "free_shipping_only": true,
  "sort_by": "PriceLowest",
  "limit": 20
}
```

Cross-field rules eBay would reject opaquely are checked up front and returned
as a readable, actionable message:

- a price bound without `price_currency`, which eBay refuses outright;
- an inverted price range;
- a `Distance` sort with no pickup origin;
- a `limit` outside 1-200, or an `offset` past 9,999;
- an `offset` that is not zero or a whole multiple of `limit` — eBay errors on
  these rather than shifting the window, which is not something a caller would
  guess;
- `Compact` detail requested from the numeric-listing-id lookup, the one
  endpoint that does not implement it.

Two further sharp edges are simply handled rather than reported. Asking for
refinement facets without `MATCHING_ITEMS` makes eBay return facets and *no
listings*, so `MATCHING_ITEMS` is added whenever it is missing. And dates are
accepted as `YYYY-MM-DD` or RFC 3339 in any offset, then normalised to the UTC
form eBay's date filters require.

A `raw_filter` escape hatch remains for anything not modelled.

## Setup

### 1. Get an eBay application keyset

Create one at [developer.ebay.com/my/keys](https://developer.ebay.com/my/keys).
You need the **App ID (Client ID)** and **Cert ID (Client Secret)** — production
and sandbox keysets are separate and not interchangeable.

Only an *application* token is used (the client-credentials grant), so no eBay
user ever has to sign in.

### 2. Configure

| Variable | Required | Default | Meaning |
| --- | --- | --- | --- |
| `EBAY_CLIENT_ID` | yes | — | App ID from the keyset |
| `EBAY_CLIENT_SECRET` | yes | — | Cert ID from the keyset |
| `EBAY_ENVIRONMENT` | no | `production` | `production` or `sandbox` |
| `EBAY_MARKETPLACE_ID` | no | `EBAY_US` | Default marketplace; any tool call may override it |
| `EBAY_BUYER_COUNTRY` | no | — | Two-letter country code; sharpens shipping costs and delivery estimates |
| `EBAY_BUYER_POSTAL_CODE` | no | — | Buyer postal code, used with the above |
| `EBAY_AFFILIATE_CAMPAIGN_ID` | no | — | eBay Partner Network campaign id; makes returned URLs affiliate links |
| `EBAY_REQUEST_TIMEOUT_SECS` | no | `30` | Per-request timeout |
| `EBAY_MCP_LOG` | no | `info` | `tracing` filter, e.g. `debug`, `ebay_browse_mcp=debug` |

Note that the sandbox has almost no listing data; a sandbox search returning
nothing is normal and not a sign of misconfiguration.

### 3. Build

``` nu
nix build
```

or, inside the dev shell:

``` nu
cargo build --release
```

### 4. Point an MCP client at it

``` json
{
  "mcpServers": {
    "ebay": {
      "command": "/path/to/ebay-browse-mcp",
      "env": {
        "EBAY_CLIENT_ID": "...",
        "EBAY_CLIENT_SECRET": "...",
        "EBAY_MARKETPLACE_ID": "EBAY_US"
      }
    }
  }
}
```

For Claude Code:

``` nu
claude mcp add ebay -e EBAY_CLIENT_ID=... -e EBAY_CLIENT_SECRET=... -- (nix build --print-out-paths --no-link | path join "bin/ebay-browse-mcp")
```

## Development

``` nu
direnv allow          # enters the dev shell and installs the git hooks
cargo test            # unit tests; no network access needed
cargo clippy --all-targets -- -D warnings
```

A live end-to-end check against the real API (needs credentials in the
environment):

``` nu
cargo test --test live -- --ignored --nocapture
```

### Layout

```
src/
  config.rs      environment-sourced configuration
  ebay/
    auth.rs      OAuth client-credentials grant with a refreshing token cache
    client.rs    HTTP, header assembly, error mapping, 401 retry
    filter.rs    typed search criteria -> eBay's `filter` mini-language
    model.rs     response types, deliberately narrower than eBay's wire format
  server.rs      MCP tool definitions
  main.rs        stdio transport wiring
```

`ebay` has no MCP dependency and is usable as a plain Browse API client.

## Notes on behaviour

- **Token handling.** An application token is minted on first use and cached
  until two minutes before it lapses. Concurrent calls that find it expired
  queue behind a single refresh rather than each minting their own. A 401
  against a supposedly-fresh token triggers exactly one re-mint and retry.
- **Response size.** eBay's raw responses are large and mostly irrelevant to an
  agent. The response types here are an explicit projection: anything not
  declared is dropped before it reaches the model's context.
- **Errors are for the model.** Failures come back as tool results with
  `is_error` set and a readable explanation, not as JSON-RPC protocol errors,
  so the model can read what went wrong and correct the call.
- **Rate limits.** eBay allows 5,000 Browse API calls per day per application
  keyset. A 429 is reported as such, with the advice to search less often and
  more specifically, rather than as a generic failure.
- **Condition granularity.** eBay's `conditions` filter accepts only `New`,
  `Used`, and `Unspecified`; the enum reflects that rather than inventing
  values eBay would reject. Finer distinctions — open box, the refurbished
  tiers, for-parts — go through `condition_ids`.

## Licence

MIT
