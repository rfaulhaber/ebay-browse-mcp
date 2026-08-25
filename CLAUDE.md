# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

An MCP server exposing eBay's Buy Browse API, built on `rmcp`. Speaks stdio by default; setting `EBAY_MCP_HTTP_ADDR` switches to Streamable HTTP behind a built-in OAuth 2.1 provider, for self-hosting as a Claude.ai custom connector.

## Commands

```
cargo build
cargo test                              # unit tests; no network needed
cargo test --lib price_without_currency # single test, by substring of its name
cargo clippy --all-targets -- -D warnings
cargo fmt --all
nix build                               # also runs the test suite in the sandbox
nix build .#oci-image                   # container image (Linux only); ./result streams a docker-archive to stdout
```

`.forgejo/workflows/build-image.yml` builds `.#oci-image` on every push to `main` (on the self-hosted Forgejo remote `home`) and skopeo-pushes it to that instance's package registry, tagged `<cargo version>-<short rev>` and retagged `latest`. It authenticates with a `REGISTRY_TOKEN` actions secret — a `write:packages` PAT — because Forgejo's automatic actions token is registry-read-only. The image defaults to the HTTP transport (`EBAY_MCP_HTTP_ADDR=0.0.0.0:8080`); the CA bundle plus `SSL_CERT_FILE` in the image are load-bearing for the same reason tests can't build a `reqwest::Client` — the cert store is probed at runtime.

`cargo-nextest` and `cargo-release` are in the dev shell. `direnv allow` enters it and installs the pre-commit hooks (clippy with `denyWarnings`, rustfmt), which run the dev-shell toolchain rather than nixpkgs' rustc so hook results match local `cargo`.

Live tests hit the real eBay API and are `#[ignore]`d. They need a keyset in the environment:

```nu
with-env {EBAY_CLIENT_ID: "...", EBAY_CLIENT_SECRET: "..."} { cargo test --test live -- --ignored --nocapture }
```

Smoke-test the MCP protocol without a client by piping JSON-RPC frames at the binary — `initialize`, then `notifications/initialized`, then `tools/list`. Dummy credentials suffice; nothing authenticates until a tool is actually called.

## Two constraints that break the build if ignored

**Tests must never construct a `reqwest::Client`.** Building one requires a system CA store, which the Nix build sandbox does not have, so `cargo test` passes locally while `nix build` fails with `No CA certificates were loaded from the system`. This is why `server_info()` is a free function rather than only a `get_info` method, and why the server tests exercise `EbayBrowseServer::tool_router()` (an associated function) instead of building a server. Keep any new test off that path.

**`nix build` only sees git-tracked files.** New files must be `git add`ed before it will pick them up, or the build fails on a missing `Cargo.lock` or missing sources.

## Crate lints shape the code

`Cargo.toml` denies `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `print_stdout`, and `print_stderr`. `clippy.toml` additionally bans `std::path::{Path, PathBuf}` (use `camino`) and most of `std::fs` (use `fs_err`).

The `print_stdout`/`print_stderr` denial is load-bearing, not stylistic: **stdout is the JSON-RPC wire**, so a stray `println!` corrupts the protocol. All diagnostics go through `tracing` to stderr, configured by `EBAY_MCP_LOG`.

Test modules therefore open with `#![allow(clippy::unwrap_used, reason = "...")]` (and `expect_used` where needed). `main` returns `ExitCode` rather than `Result` so startup failures are reported through the logger — a `Result`-returning `main` prints the error's `Debug` form and would show `Config(MissingVar("EBAY_CLIENT_ID"))` instead of the message explaining the fix.

## Architecture

`ebay/` has no MCP dependency and works as a standalone Browse client; `server.rs` binds it to tools. Keep that boundary.

- `config.rs` — environment-sourced config. `Secret` has a redacting `Debug`.
- `ebay/auth.rs` — OAuth client-credentials grant. The token cache deliberately holds the write lock across the network fetch so concurrent callers queue behind one refresh. A 401 against a supposedly-fresh token triggers exactly one re-mint and retry (in `client.rs`).
- `ebay/filter.rs` — the heart of the design, and the largest file.
- `ebay/model.rs` — response types.
- `ebay/client.rs` — HTTP, headers, error mapping.
- `server.rs` — five tools, plus the `INSTRUCTIONS` string sent at connection time.
- `oauth.rs` — single-operator OAuth 2.1 provider for remote use. Distinct from `ebay/auth.rs`: there this server is an OAuth *client* toward eBay; here it is an authorization server + resource server toward Claude. Every artifact (client id, code, tokens) is a stateless HMAC-sealed `Claims` blob — domain separation comes from the serde `typ` tag, revocation is "rotate `EBAY_MCP_TOKEN_KEY`".
- `remote.rs` — axum router: RFC 9728/8414 discovery documents, DCR, consent page, token endpoint, and the bearer gate in front of rmcp's `StreamableHttpService`. `router()` is generic over the MCP service so tests gate a stub — this is how the remote tests stay off the `reqwest::Client` path.

### Remote-transport facts verified against Anthropic's docs (2026-08)

- Claude only starts OAuth discovery from a real **401** whose `WWW-Authenticate` carries `resource_metadata`; a challenge on a 200 is ignored.
- The protected-resource metadata `resource` must **byte-match** the connector URL as entered in Claude — hence `EBAY_MCP_PUBLIC_URL` is kept verbatim, never normalized.
- Claude registers a fresh DCR client per connection (hence stateless client ids), always uses PKCE S256, requests `offline_access` only because the AS metadata advertises it, and calls back to `https://claude.ai/api/mcp/auth_callback` (hosted) or port-agnostic `http://localhost/…` loopback (Claude Code).
- Claude requires a public HTTPS hostname (it refuses names resolving to private/loopback addresses) and waits ≤10 s on OAuth endpoints. TLS terminates at a reverse proxy, which must forward the whole vhost: discovery and OAuth endpoints share the MCP origin.
- rmcp 3.1.3 has **no server-side auth**; its `StreamableHttpServerConfig` is `#[non_exhaustive]` (use `::default().with_allowed_hosts(…)`), and its default `allowed_hosts` is localhost-only — the public hostname must be added or every proxied request is rejected.

### The filter layer is the point of the project

eBay packs most search criteria into one `filter` query parameter written in a mini-language: comma-separated `name:value`, ranges as `[low..high]` (either bound omittable), sets as `{A|B|C}`. A model asked to write that string directly produces HTTP 400s.

So `SearchRequest` models every criterion as a typed, enumerated field and renders the DSL in one place. **Prefer adding a typed field over widening `raw_filter`.** Cross-field rules that JSON Schema cannot express are validated in `validate()`/`validate_constraints()` and returned as messages that state the remedy, because the model reads them and retries.

### Response models are a deliberate projection

eBay's `ItemSummary` has 52 fields and `Item` has 73; a 200-result page of raw JSON would burn enormous context. The structs in `model.rs` declare only what an agent uses, and serde drops the rest — **the struct definition is the projection**. Adding a field means paying for it on every result. Money is a `String` on the wire (`"19.99"`), not a number.

### Errors are for the model, not the protocol

Tools return `Result<Json<T>, String>`. Reading rmcp's `IntoCallToolResult` impls: `Err(ErrorData)` becomes a JSON-RPC *protocol* error, while `Err(String)` becomes a normal tool result with `is_error: true` and the text as content — which the model reads and can correct from. For failures like "price without a currency" that is the difference between self-correction and a dead end. `describe()` in `server.rs` appends a remedy to each error class.

`Json<T>` populates both `structuredContent` and a text `content` block, so responses work on clients that don't understand structured output.

## eBay API facts verified against the contract

These were each confirmed from eBay's own OpenAPI spec (v1.20.4) or its field-filter reference. Several are counter-intuitive and were bugs before verification — do not "simplify" them away:

- **`conditions` accepts only `NEW`, `USED`, `UNSPECIFIED`.** Refurbished tiers are *not* valid here; they are numeric `condition_ids` (2000/2010/2020/2030).
- **`MATCHING_ITEMS` is not implied.** Requesting refinement facets alone returns facets and *no listings*, so it is injected automatically unless `FULL` is present.
- **`offset` must be zero or an exact multiple of `limit`** (default 50). eBay errors otherwise rather than shifting the window. `limit` is 1–200, `offset` maxes at 9,999.
- **Date filters take `YYYY-MM-DDTHH:MM:SSZ`** — no milliseconds.
- **`getItemByLegacyId` does not support the `COMPACT` field group**, though `getItem` does. The item endpoints take four groups: `PRODUCT`, `COMPACT`, `ADDITIONAL_SELLER_DETAILS`, `CHARITY_DETAILS`.
- **An invalid `category_ids` does not fail the search.** eBay documents 12009 as a *warning*: the bad parameter is dropped and the search returns HTTP 200 with listings anyway. The same goes for invalid filter values (12002) — the documented 400s for `search` (bad `limit`, `offset` off a page boundary) are all pre-empted client-side by `validate_constraints()`, so a live eBay 400 is effectively unreachable through `SearchRequest`. Rejections must be observed via `warnings`, not `Err`.
- `aspect_filter` requires a leading `categoryId`; aspects without a category are dropped rather than sent invalid.
- `price` is rejected without `priceCurrency`.
- Item ids embed `|` (`v1|254188828753|0`) and must be percent-encoded into the path.
- Rate limit: 5,000 calls/day per application keyset.
- The shopping-cart methods older docs mention are no longer part of the Browse API.

### Re-verifying eBay's docs

`developer.ebay.com` returns 403 to curl and times out for WebFetch, so the docs are not directly readable. What works:

- eBay's OpenAPI contract via a verbatim GitHub mirror — find the newest with `gh api -X GET search/code -f q='buy_browse_v1_oas3 in:path'` and **check `.info.version`**, as mirrors range from v1.8.0 to v1.20.4 and the stale ones disagree on real details.
- The field-filter reference (which the OAS only links to) via `web.archive.org` with a browser `User-Agent`. WebFetch cannot reach archive.org; plain `curl` can.

## rmcp specifics (3.1.3)

Verified against the vendored crate source in `~/.cargo/registry/src/*/rmcp-3.1.3/`, which is more reliable than tutorials — the API moved across releases.

- `Parameters<T>` lives at `rmcp::handler::server::wrapper::Parameters`, *not* `...::tool::Parameters`.
- The content type is `ContentBlock`; a bare `Content` does not exist (pre-3.0 naming). The error type is `ErrorData`; `rmcp::Error` does not exist.
- `ServerCapabilities` and `InitializeResult` are `#[non_exhaustive]` — build via `::builder()` / `::new()`, not struct literals.
- Tool descriptions and per-field schema descriptions come from `///` doc comments, including per-variant docs on enums, which reach the model as `oneOf` entries. Write them for the model.
- `#[tool_handler(router = self.tool_router)]` uses the stored router; bare `#[tool_handler]` rebuilds it via `Self::tool_router()` on every call.
- Do **not** use `Implementation::from_build_env()` — its `env!` calls resolve against rmcp's own crate metadata and announce the server as `rmcp/3.1.3`.
- `schemars` must be 1.x; a 0.8 `JsonSchema` derive will not satisfy the trait bound.

Ryan's shell is nushell on every machine, including remote hosts — any command written for him to run must be nu syntax.
