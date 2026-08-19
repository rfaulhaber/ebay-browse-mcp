//! An MCP server exposing eBay's Buy Browse API.
//!
//! The crate is layered so the eBay client is usable on its own:
//!
//! - [`config`] reads credentials and defaults from the environment.
//! - [`ebay`] is a typed Browse API client — OAuth, request building, and
//!   trimmed response models — with no MCP dependency.
//! - [`server`] binds that client to MCP tools.

pub mod config;
pub mod ebay;
pub mod server;

pub use config::Config;
pub use server::EbayBrowseServer;
