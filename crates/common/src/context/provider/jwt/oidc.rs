use oidc_jwt_validator::{ValidationError, ValidationSettings, Validator, cache::Strategy};
use serde_json::Value;
use std::collections::HashSet;

use super::authenticator::JwtConfigurationError;

pub struct Oidc {
    validator: Validator,
}

impl Oidc {
    pub(super) async fn new(
        url: String,
        allowed_audiences: Option<Vec<String>>,
        issuer_aliases: Vec<String>,
    ) -> Result<Self, JwtConfigurationError> {
        let client = reqwest::ClientBuilder::new().build().map_err(|e| {
            JwtConfigurationError::Configuration {
                message: "Unable to create HTTP client".to_owned(),
                source: e.into(),
            }
        })?;
        let mut settings = ValidationSettings::new();

        // The issuer can be either the base URL (for example, Clerk) or the base URL with a trailing slash (for example, Auth0)
        // so we add both to the list of issuers to check, plus any additional aliases supplied via configuration.
        let base_url = url.trim_end_matches('/').to_owned();
        let mut issuers = Vec::new();
        let mut seen = HashSet::new();

        let mut add_issuer = |candidate: String| {
            if seen.insert(candidate.clone()) {
                issuers.push(candidate);
            }
        };

        add_issuer(base_url.clone());
        add_issuer(format!("{base_url}/"));

        for alias in issuer_aliases {
            let normalized = alias.trim().trim_end_matches('/').to_string();
            if normalized.is_empty() {
                continue;
            }
            add_issuer(normalized.clone());
            add_issuer(format!("{normalized}/"));
        }

        settings.set_issuer(&issuers);
        if let Some(audiences) = &allowed_audiences {
            settings.set_audience(audiences);
        }

        let validator = Validator::new(base_url, client, Strategy::Automatic, settings)
            .await
            .map_err(|e| JwtConfigurationError::Configuration {
                message: "Unable to create validator".to_owned(),
                source: e.into(),
            })?;

        Ok(Self { validator })
    }

    pub(super) async fn validate(&self, token: &str) -> Result<Value, ValidationError> {
        Ok(self.validator.validate(token).await?.claims)
    }
}
