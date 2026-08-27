//! An MCP server exposing eBay's Buy Browse API.
//!
//! The crate is layered so the eBay client is usable on its own:
//!
//! - [`config`] reads credentials and defaults from the environment.
//! - [`ebay`] is a typed Browse API client — OAuth, request building, and
//!   trimmed response models — with no MCP dependency.
//! - [`server`] binds that client to MCP tools.
//! - [`oauth`] is a single-operator OAuth 2.1 provider for remote use, and
//!   [`remote`] serves the MCP tools over Streamable HTTP behind it.
//! - [`account_deletion`] answers eBay's marketplace-account-deletion
//!   webhook, a compliance requirement for production keysets.

pub mod account_deletion;
pub mod config;
pub mod ebay;
pub mod oauth;
pub mod remote;
pub mod server;

pub use config::Config;
pub use server::EbayBrowseServer;
