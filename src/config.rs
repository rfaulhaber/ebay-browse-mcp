//! Runtime configuration, sourced from the process environment.

use std::{fmt, time::Duration};

use crate::ebay::Marketplace;

/// eBay operates two independent deployments with separate credentials; a
/// sandbox key is rejected outright by production and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Environment {
    #[default]
    Production,
    Sandbox,
}

impl Environment {
    /// Host serving both the OAuth token endpoint and the Browse API.
    #[must_use]
    pub const fn host(self) -> &'static str {
        match self {
            Self::Production => "https://api.ebay.com",
            Self::Sandbox => "https://api.sandbox.ebay.com",
        }
    }

    #[must_use]
    pub fn token_url(self) -> String {
        format!("{}/identity/v1/oauth2/token", self.host())
    }

    #[must_use]
    pub fn browse_base_url(self) -> String {
        format!("{}/buy/browse/v1", self.host())
    }
}

/// A credential that must never reach a log line or an error message.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub client_id: String,
    pub client_secret: Secret,
    pub environment: Environment,
    /// Marketplace used when a tool call does not override it.
    pub default_marketplace: Marketplace,
    /// Buyer location passed as `X-EBAY-C-ENDUSERCTX`. Improves shipping-cost
    /// and delivery-estimate accuracy, and is required for some `filter`
    /// fields to have any effect.
    pub contextual_location: Option<ContextualLocation>,
    /// eBay Partner Network campaign id. When set, item URLs in responses come
    /// back as affiliate links.
    pub affiliate_campaign_id: Option<String>,
    pub request_timeout: Duration,
}

/// Buyer's country (and optionally postal code) for the `X-EBAY-C-ENDUSERCTX`
/// header.
#[derive(Debug, Clone)]
pub struct ContextualLocation {
    pub country: String,
    pub zip: Option<String>,
}

impl ContextualLocation {
    /// eBay expects `contextualLocation=country=US,zip=19406`, with the inner
    /// pairs percent-encoded because they nest inside a header value that is
    /// itself comma-delimited.
    #[must_use]
    pub fn to_header_value(&self) -> String {
        match &self.zip {
            Some(zip) => format!("country%3D{}%2Czip%3D{}", self.country, zip),
            None => format!("country%3D{}", self.country),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "{0} is not set; create an application keyset at \
         https://developer.ebay.com/my/keys and export its App ID (Client ID) \
         and Cert ID (Client Secret)"
    )]
    MissingVar(&'static str),
    #[error("{var} has an invalid value {value:?}: {reason}")]
    InvalidVar {
        var: &'static str,
        value: String,
        reason: String,
    },
}

impl Config {
    /// Build configuration from the environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a required variable is absent or a
    /// recognised variable cannot be parsed.
    pub fn from_env() -> Result<Self, ConfigError> {
        let client_id = required("EBAY_CLIENT_ID")?;
        let client_secret = Secret::new(required("EBAY_CLIENT_SECRET")?);

        let environment = match optional("EBAY_ENVIRONMENT").as_deref() {
            None | Some("production" | "prod") => Environment::Production,
            Some("sandbox") => Environment::Sandbox,
            Some(other) => {
                return Err(ConfigError::InvalidVar {
                    var: "EBAY_ENVIRONMENT",
                    value: other.to_owned(),
                    reason: "expected \"production\" or \"sandbox\"".to_owned(),
                });
            }
        };

        let default_marketplace = match optional("EBAY_MARKETPLACE_ID") {
            None => Marketplace::default(),
            Some(raw) => raw
                .parse()
                .map_err(|reason: String| ConfigError::InvalidVar {
                    var: "EBAY_MARKETPLACE_ID",
                    value: raw,
                    reason,
                })?,
        };

        let contextual_location =
            optional("EBAY_BUYER_COUNTRY").map(|country| ContextualLocation {
                country,
                zip: optional("EBAY_BUYER_POSTAL_CODE"),
            });

        let request_timeout = match optional("EBAY_REQUEST_TIMEOUT_SECS") {
            None => Duration::from_secs(30),
            Some(raw) => raw.parse::<u64>().map(Duration::from_secs).map_err(|err| {
                ConfigError::InvalidVar {
                    var: "EBAY_REQUEST_TIMEOUT_SECS",
                    value: raw,
                    reason: err.to_string(),
                }
            })?,
        };

        Ok(Self {
            client_id,
            client_secret,
            environment,
            default_marketplace,
            contextual_location,
            affiliate_campaign_id: optional("EBAY_AFFILIATE_CAMPAIGN_ID"),
            request_timeout,
        })
    }
}

fn required(var: &'static str) -> Result<String, ConfigError> {
    optional(var).ok_or(ConfigError::MissingVar(var))
}

/// Treats an empty or whitespace-only value as unset, so a `FOO=` line in a
/// launcher's env block does not masquerade as configuration.
fn optional(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(!format!("{secret:?}").contains("hunter2"));
    }

    #[test]
    fn environment_urls() {
        assert_eq!(
            Environment::Production.token_url(),
            "https://api.ebay.com/identity/v1/oauth2/token"
        );
        assert_eq!(
            Environment::Sandbox.browse_base_url(),
            "https://api.sandbox.ebay.com/buy/browse/v1"
        );
    }

    #[test]
    fn contextual_location_encodes_inner_delimiters() {
        let loc = ContextualLocation {
            country: "US".to_owned(),
            zip: Some("19406".to_owned()),
        };
        assert_eq!(loc.to_header_value(), "country%3DUS%2Czip%3D19406");

        let loc = ContextualLocation {
            country: "DE".to_owned(),
            zip: None,
        };
        assert_eq!(loc.to_header_value(), "country%3DDE");
    }
}
