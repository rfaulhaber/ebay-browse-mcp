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
    #[error("{var} is not set, but EBAY_MCP_HTTP_ADDR enables the HTTP transport; {remedy}")]
    MissingHttpVar {
        var: &'static str,
        remedy: &'static str,
    },
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

/// Configuration for the remote (Streamable HTTP + OAuth) transport. Absent
/// unless `EBAY_MCP_HTTP_ADDR` is set, in which case the rest becomes
/// required.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Local listen address, e.g. `0.0.0.0:8080`.
    pub bind: std::net::SocketAddr,
    /// The MCP endpoint URL exactly as it will be entered into the MCP
    /// client. RFC 9728 requires the advertised `resource` to byte-match it,
    /// so it is kept verbatim rather than normalized.
    pub public_url: String,
    /// Origin of `public_url`; OAuth endpoints and discovery documents live
    /// there. TLS is expected to terminate at a reverse proxy in front of
    /// `bind`.
    pub issuer: String,
    /// Path component of `public_url`, where the MCP service is mounted.
    pub mcp_path: String,
    /// Host component of `public_url`, for the transport's Host-header
    /// (DNS-rebinding) validation.
    pub public_host: String,
    /// Shared secret the operator types on the consent page.
    pub access_key: Secret,
    /// HMAC key sealing every OAuth artifact. Rotating it revokes all of
    /// them at once.
    pub token_key: Secret,
}

impl HttpConfig {
    /// Reads the HTTP transport configuration, or `None` when
    /// `EBAY_MCP_HTTP_ADDR` is unset and the server should speak stdio.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the transport is enabled but a required
    /// variable is absent or unparseable.
    pub fn from_env() -> Result<Option<Self>, ConfigError> {
        let Some(addr) = optional("EBAY_MCP_HTTP_ADDR") else {
            return Ok(None);
        };
        let bind = addr
            .parse::<std::net::SocketAddr>()
            .map_err(|err| ConfigError::InvalidVar {
                var: "EBAY_MCP_HTTP_ADDR",
                value: addr.clone(),
                reason: format!("{err}; expected an address like 0.0.0.0:8080"),
            })?;

        let public_url = optional("EBAY_MCP_PUBLIC_URL").ok_or(ConfigError::MissingHttpVar {
            var: "EBAY_MCP_PUBLIC_URL",
            remedy: "set it to the MCP endpoint URL exactly as it will be entered \
                         into the client, e.g. https://mcp.example.com/mcp",
        })?;
        let parsed = url::Url::parse(&public_url).map_err(|err| ConfigError::InvalidVar {
            var: "EBAY_MCP_PUBLIC_URL",
            value: public_url.clone(),
            reason: err.to_string(),
        })?;
        let invalid_url = |reason: &str| ConfigError::InvalidVar {
            var: "EBAY_MCP_PUBLIC_URL",
            value: public_url.clone(),
            reason: reason.to_owned(),
        };
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(invalid_url("the scheme must be http or https"));
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(invalid_url(
                "a query or fragment cannot be part of the MCP endpoint",
            ));
        }
        let public_host = parsed
            .host_str()
            .ok_or_else(|| invalid_url("a host is required"))?
            .to_owned();
        let issuer = parsed.origin().ascii_serialization();
        let mcp_path = parsed.path().to_owned();

        let access_key = optional("EBAY_MCP_ACCESS_KEY").ok_or(ConfigError::MissingHttpVar {
            var: "EBAY_MCP_ACCESS_KEY",
            remedy: "set it to the secret (16+ characters) the operator will type on the \
                     OAuth consent page",
        })?;
        if access_key.len() < 16 {
            return Err(ConfigError::InvalidVar {
                var: "EBAY_MCP_ACCESS_KEY",
                value: "<redacted>".to_owned(),
                reason: "must be at least 16 characters".to_owned(),
            });
        }

        let token_key = optional("EBAY_MCP_TOKEN_KEY").ok_or(ConfigError::MissingHttpVar {
            var: "EBAY_MCP_TOKEN_KEY",
            remedy: "set it to at least 32 random characters (e.g. from `openssl rand \
                     -base64 32`); rotating it revokes every issued token",
        })?;
        if token_key.len() < 32 {
            return Err(ConfigError::InvalidVar {
                var: "EBAY_MCP_TOKEN_KEY",
                value: "<redacted>".to_owned(),
                reason: "must be at least 32 characters".to_owned(),
            });
        }

        Ok(Some(Self {
            bind,
            public_url,
            issuer,
            mcp_path,
            public_host,
            access_key: Secret::new(access_key),
            token_key: Secret::new(token_key),
        }))
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
