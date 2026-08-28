//! A thin, typed client for eBay's Buy Browse API.

pub mod auth;
pub mod client;
pub mod filter;
mod html;
pub mod model;

use std::{fmt, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use client::BrowseClient;

/// eBay marketplaces on which the Browse API is available.
///
/// The marketplace selects the site a search runs against and, with it, the
/// currency, language, and catalogue. It is sent as the
/// `X-EBAY-C-MARKETPLACE-ID` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub enum Marketplace {
    /// United States (ebay.com)
    #[default]
    #[serde(rename = "EBAY_US")]
    US,
    /// Austria (ebay.at)
    #[serde(rename = "EBAY_AT")]
    AT,
    /// Australia (ebay.com.au)
    #[serde(rename = "EBAY_AU")]
    AU,
    /// Belgium (ebay.be)
    #[serde(rename = "EBAY_BE")]
    BE,
    /// Canada (ebay.ca)
    #[serde(rename = "EBAY_CA")]
    CA,
    /// Switzerland (ebay.ch)
    #[serde(rename = "EBAY_CH")]
    CH,
    /// Germany (ebay.de)
    #[serde(rename = "EBAY_DE")]
    DE,
    /// Spain (ebay.es)
    #[serde(rename = "EBAY_ES")]
    ES,
    /// France (ebay.fr)
    #[serde(rename = "EBAY_FR")]
    FR,
    /// United Kingdom (ebay.co.uk)
    #[serde(rename = "EBAY_GB")]
    GB,
    /// Hong Kong (ebay.com.hk)
    #[serde(rename = "EBAY_HK")]
    HK,
    /// Ireland (ebay.ie)
    #[serde(rename = "EBAY_IE")]
    IE,
    /// Italy (ebay.it)
    #[serde(rename = "EBAY_IT")]
    IT,
    /// Malaysia (ebay.com.my)
    #[serde(rename = "EBAY_MY")]
    MY,
    /// Netherlands (ebay.nl)
    #[serde(rename = "EBAY_NL")]
    NL,
    /// Philippines (ebay.ph)
    #[serde(rename = "EBAY_PH")]
    PH,
    /// Poland (ebay.pl)
    #[serde(rename = "EBAY_PL")]
    PL,
    /// Singapore (ebay.com.sg)
    #[serde(rename = "EBAY_SG")]
    SG,
    /// Taiwan (ebay.com.tw)
    #[serde(rename = "EBAY_TW")]
    TW,
}

impl Marketplace {
    #[must_use]
    pub const fn as_header_value(self) -> &'static str {
        match self {
            Self::US => "EBAY_US",
            Self::AT => "EBAY_AT",
            Self::AU => "EBAY_AU",
            Self::BE => "EBAY_BE",
            Self::CA => "EBAY_CA",
            Self::CH => "EBAY_CH",
            Self::DE => "EBAY_DE",
            Self::ES => "EBAY_ES",
            Self::FR => "EBAY_FR",
            Self::GB => "EBAY_GB",
            Self::HK => "EBAY_HK",
            Self::IE => "EBAY_IE",
            Self::IT => "EBAY_IT",
            Self::MY => "EBAY_MY",
            Self::NL => "EBAY_NL",
            Self::PH => "EBAY_PH",
            Self::PL => "EBAY_PL",
            Self::SG => "EBAY_SG",
            Self::TW => "EBAY_TW",
        }
    }

    /// Every supported marketplace, for building help text.
    pub const ALL: [Self; 19] = [
        Self::US,
        Self::AT,
        Self::AU,
        Self::BE,
        Self::CA,
        Self::CH,
        Self::DE,
        Self::ES,
        Self::FR,
        Self::GB,
        Self::HK,
        Self::IE,
        Self::IT,
        Self::MY,
        Self::NL,
        Self::PH,
        Self::PL,
        Self::SG,
        Self::TW,
    ];
}

impl fmt::Display for Marketplace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_header_value())
    }
}

impl FromStr for Marketplace {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_uppercase();
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_header_value() == normalized)
            .ok_or_else(|| {
                let supported = Self::ALL
                    .iter()
                    .map(|m| m.as_header_value())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unsupported marketplace; expected one of: {supported}")
            })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "panicking is how a test reports failure"
    )]

    use super::*;

    #[test]
    fn marketplace_round_trips_through_str() {
        for marketplace in Marketplace::ALL {
            assert_eq!(
                marketplace
                    .as_header_value()
                    .parse::<Marketplace>()
                    .unwrap(),
                marketplace
            );
        }
    }

    #[test]
    fn marketplace_parse_is_case_insensitive() {
        assert_eq!("ebay_gb".parse::<Marketplace>().unwrap(), Marketplace::GB);
    }

    #[test]
    fn marketplace_parse_rejects_unknown_and_lists_alternatives() {
        let err = "EBAY_MARS".parse::<Marketplace>().unwrap_err();
        assert!(
            err.contains("EBAY_US"),
            "error should enumerate options: {err}"
        );
    }

    #[test]
    fn serde_uses_ebay_site_ids() {
        assert_eq!(
            serde_json::to_string(&Marketplace::DE).unwrap(),
            "\"EBAY_DE\""
        );
        assert_eq!(
            serde_json::from_str::<Marketplace>("\"EBAY_GB\"").unwrap(),
            Marketplace::GB
        );
    }
}
