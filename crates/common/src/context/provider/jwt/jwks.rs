use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, warn};

use super::authenticator::{JwtConfigurationError, jwt_debug_enabled, jwt_debug_log};

/// Do not re-fetch the JWKS more often than this when tokens arrive with
/// unknown kids. The first unknown kid after startup always triggers a fetch;
/// this interval only throttles subsequent attempts, so a burst of garbage
/// kids cannot turn the validator into a JWKS-fetching loop.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Serialize, Deserialize)]
struct Jwks {
    keys: Vec<JwkKey>,
}

/// A single JWK. All key-material fields are optional so that one unsupported
/// or exotic key in the set does not fail deserialization of the whole JWKS;
/// unusable keys are skipped with a warning instead.
#[derive(Debug, Serialize, Deserialize)]
struct JwkKey {
    #[serde(rename = "kty")]
    key_type: String,
    #[serde(rename = "use")]
    key_use: Option<String>,
    kid: Option<String>,
    alg: Option<String>,
    // RSA components
    n: Option<String>,
    e: Option<String>,
    // OKP (EdDSA) components
    crv: Option<String>,
    x: Option<String>,
}

impl JwkKey {
    /// Build a decoding key and the algorithm to validate with. Returns a
    /// human-readable reason when the key cannot be used.
    fn to_decoding_key(&self) -> Result<(DecodingKey, Algorithm), String> {
        match self.key_type.as_str() {
            "RSA" => {
                let n = self.n.as_deref().ok_or("RSA key is missing 'n'")?;
                let e = self.e.as_deref().ok_or("RSA key is missing 'e'")?;
                let algorithm = match self.alg.as_deref() {
                    None | Some("RS256") => Algorithm::RS256,
                    Some("RS384") => Algorithm::RS384,
                    Some("RS512") => Algorithm::RS512,
                    Some(other) => return Err(format!("unsupported RSA algorithm '{other}'")),
                };
                DecodingKey::from_rsa_components(n, e)
                    .map(|key| (key, algorithm))
                    .map_err(|e| format!("invalid RSA components: {e}"))
            }
            "OKP" => match self.crv.as_deref() {
                Some("Ed25519") => {
                    let x = self.x.as_deref().ok_or("OKP key is missing 'x'")?;
                    DecodingKey::from_ed_components(x)
                        .map(|key| (key, Algorithm::EdDSA))
                        .map_err(|e| format!("invalid Ed25519 component: {e}"))
                }
                other => Err(format!("unsupported OKP curve {other:?}")),
            },
            other => Err(format!("unsupported key type '{other}'")),
        }
    }
}

pub struct JwksValidator {
    jwks_url: String,
    keys: RwLock<HashMap<String, (DecodingKey, Algorithm)>>,
    client: reqwest::Client,
    allowed_audiences: Option<Vec<String>>,
    allowed_issuers: Option<Vec<String>>,
    /// When the JWKS was last fetched because of an unknown kid. `None` until
    /// the first such fetch, so a rotation right after startup is picked up
    /// immediately.
    last_miss_refresh: Mutex<Option<Instant>>,
}

impl JwksValidator {
    pub async fn new_with_audiences(
        jwks_url: String,
        allowed_audiences: Option<Vec<String>>,
    ) -> Result<Self, JwtConfigurationError> {
        Self::new_with_config(jwks_url, allowed_audiences, None).await
    }

    pub async fn new_with_config(
        jwks_url: String,
        allowed_audiences: Option<Vec<String>>,
        allowed_issuers: Option<Vec<String>>,
    ) -> Result<Self, JwtConfigurationError> {
        let client = reqwest::ClientBuilder::new().build().map_err(|e| {
            JwtConfigurationError::Configuration {
                message: "Unable to create HTTP client".to_owned(),
                source: e.into(),
            }
        })?;

        let normalized_issuers = allowed_issuers.map(|issuers| {
            let mut seen = HashSet::new();
            let mut normalized = Vec::new();
            for issuer in issuers {
                let trimmed = issuer.trim().trim_end_matches('/').to_string();
                if trimmed.is_empty() {
                    continue;
                }
                if seen.insert(trimmed.clone()) {
                    normalized.push(trimmed);
                }
            }
            normalized
        });

        let validator = Self {
            jwks_url: jwks_url.clone(),
            keys: RwLock::new(HashMap::new()),
            client: client.clone(),
            allowed_audiences,
            allowed_issuers: normalized_issuers,
            last_miss_refresh: Mutex::new(None),
        };

        // Fetch initial keys; startup fails if the JWKS is unreachable or
        // holds no usable key, as before.
        let initial_keys = validator.fetch_keys().await?;
        *validator.keys.write().await = initial_keys;

        jwt_debug_log(|| {
            let kids = validator.debug_known_kids();
            format!(
                "Initialized JWKS provider '{}' with {} key(s); kids={:?}; audience_filter={:?}; issuer_filter={:?}",
                jwks_url,
                kids.len(),
                kids,
                validator.allowed_audiences.as_ref(),
                validator.allowed_issuers.as_ref()
            )
        });

        Ok(validator)
    }

    async fn fetch_keys(
        &self,
    ) -> Result<HashMap<String, (DecodingKey, Algorithm)>, JwtConfigurationError> {
        let response = self.client.get(&self.jwks_url).send().await.map_err(|e| {
            JwtConfigurationError::Configuration {
                message: format!("Failed to fetch JWKS from {}", self.jwks_url),
                source: e.into(),
            }
        })?;

        let jwks: Jwks =
            response
                .json()
                .await
                .map_err(|e| JwtConfigurationError::Configuration {
                    message: "Failed to parse JWKS response".to_owned(),
                    source: e.into(),
                })?;

        let mut new_keys = HashMap::new();
        for key in jwks.keys {
            let kid = key.kid.clone().unwrap_or_else(|| "default".to_string());

            match key.to_decoding_key() {
                Ok(decoding_key) => {
                    new_keys.insert(kid, decoding_key);
                }
                Err(reason) => {
                    warn!("Skipping unusable JWKS key '{}': {}", kid, reason);
                }
            }
        }

        if new_keys.is_empty() {
            return Err(JwtConfigurationError::Configuration {
                message: format!(
                    "No usable signing keys (RSA or Ed25519) found in JWKS from {}",
                    self.jwks_url
                ),
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "No valid keys",
                )),
            });
        }

        Ok(new_keys)
    }

    async fn lookup_key(&self, kid: &str) -> Option<(DecodingKey, Algorithm)> {
        self.keys
            .read()
            .await
            .get(kid)
            .map(|(key, algorithm)| (key.clone(), *algorithm))
    }

    /// Called when a token carries a kid we do not know: re-fetch the JWKS so
    /// a provider-side key rotation does not require a process restart. The
    /// fetch is single-flight (concurrent misses wait on the mutex, then see
    /// the refreshed map) and throttled to one attempt per
    /// `MIN_REFRESH_INTERVAL`. A failed or empty fetch keeps the current keys.
    async fn refresh_after_miss(&self, missing_kid: &str) {
        let mut last_refresh = self.last_miss_refresh.lock().await;

        if let Some(last) = *last_refresh
            && last.elapsed() < MIN_REFRESH_INTERVAL
        {
            return;
        }
        *last_refresh = Some(Instant::now());

        info!(
            "JWKS '{}' has no key for kid '{}'; re-fetching keys",
            self.jwks_url, missing_kid
        );

        match self.fetch_keys().await {
            Ok(new_keys) => {
                let kid_count = new_keys.len();
                let kids: Vec<String> = new_keys.keys().cloned().collect();
                *self.keys.write().await = new_keys;
                info!(
                    "JWKS '{}' re-fetched: {} key(s), kids={:?}",
                    self.jwks_url, kid_count, kids
                );
            }
            Err(e) => {
                // Keep serving the keys we already have.
                error!(
                    "JWKS '{}' re-fetch after unknown kid '{}' failed: {}",
                    self.jwks_url, missing_kid, e
                );
            }
        }
    }

    pub async fn validate(&self, token: &str) -> Result<Value, JwtValidationError> {
        // Decode header to get kid
        let header = decode_header(token).map_err(|e| {
            error!("Failed to decode JWT header: {}", e);
            JwtValidationError::Invalid
        })?;

        let kid = header.kid.unwrap_or_else(|| "default".to_string());

        // Get the decoding key for this kid, re-fetching the JWKS once if the
        // kid is unknown (the provider may have rotated its keys).
        let mut key_entry = self.lookup_key(&kid).await;
        if key_entry.is_none() {
            self.refresh_after_miss(&kid).await;
            key_entry = self.lookup_key(&kid).await;
        }

        let (decoding_key, algorithm) = key_entry.ok_or_else(|| {
            error!("No key found for kid: {}", kid);
            if jwt_debug_enabled() {
                let available = self.debug_known_kids();
                eprintln!(
                    "[JWT Debug] JWKS '{}' does not contain kid '{}'. Available kids: {:?}",
                    self.jwks_url, kid, available
                );
            }
            JwtValidationError::Invalid
        })?;

        // Create validation settings
        let mut validation = Validation::new(algorithm);
        validation.validate_exp = true;
        validation.validate_nbf = false;

        // Configure audience validation based on settings
        if let Some(audiences) = &self.allowed_audiences {
            validation.set_audience(audiences);
            validation.validate_aud = true;
        } else {
            // If no audiences configured, skip audience validation
            validation.validate_aud = false;
        }

        validation.set_required_spec_claims::<&str>(&[]); // Don't require iss claim

        // Decode and validate token
        if jwt_debug_enabled() {
            eprintln!(
                "[JWKS Validator] Attempting to validate with kid: {} (alg: {:?})",
                kid, algorithm
            );
            eprintln!(
                "[JWKS Validator] Validation settings: exp={}, nbf={}, aud={}",
                validation.validate_exp, validation.validate_nbf, validation.validate_aud
            );
        }

        let token_data = decode::<Value>(token, &decoding_key, &validation).map_err(|e| {
            if jwt_debug_enabled() {
                eprintln!("[JWKS Validator] Validation failed: {:?}", e);
            }
            match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                    error!("JWT validation failed: expired signature");
                    if jwt_debug_enabled() {
                        eprintln!(
                            "[JWT Debug] JWKS '{}' reports expired token while validating kid '{}'",
                            self.jwks_url, kid
                        );
                    }
                    JwtValidationError::Expired
                }
                _ => {
                    error!("JWT validation failed: {}", e);
                    if jwt_debug_enabled() {
                        eprintln!(
                            "[JWT Debug] JWKS '{}' failed to validate kid '{}': {}",
                            self.jwks_url, kid, e
                        );
                    }
                    JwtValidationError::Invalid
                }
            }
        })?;

        if jwt_debug_enabled() {
            eprintln!("[JWKS Validator] Validation successful!");
        }

        let claims = token_data.claims;

        if let Some(issuers) = &self.allowed_issuers {
            let issuer_value = claims
                .get("iss")
                .and_then(|value| value.as_str())
                .map(|value| value.trim().trim_end_matches('/').to_string());

            let issuer_matches = issuer_value
                .as_ref()
                .map(|candidate| issuers.iter().any(|allowed| allowed == candidate))
                .unwrap_or(false);

            if !issuer_matches {
                error!(
                    "JWT validation failed: issuer {:?} not allowed for provider {}",
                    issuer_value, self.jwks_url
                );
                if jwt_debug_enabled() {
                    eprintln!(
                        "[JWT Debug] JWKS '{}' rejected issuer {:?}; allowed issuers: {:?}",
                        self.jwks_url, issuer_value, issuers
                    );
                }
                return Err(JwtValidationError::Invalid);
            }
        }

        Ok(claims)
    }
}

impl JwksValidator {
    pub fn debug_source(&self) -> &str {
        &self.jwks_url
    }

    pub fn debug_known_kids(&self) -> Vec<String> {
        // Best-effort: used only for debug logging. `try_read` avoids
        // blocking a sync caller; contention just yields an empty list.
        match self.keys.try_read() {
            Ok(keys) => keys.keys().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn debug_allowed_audiences(&self) -> Option<&[String]> {
        self.allowed_audiences.as_deref()
    }

    pub fn debug_allowed_issuers(&self) -> Option<&[String]> {
        self.allowed_issuers.as_deref()
    }
}

#[derive(Debug)]
pub enum JwtValidationError {
    Invalid,
    Expired,
    KidMismatch,
}

impl std::fmt::Display for JwtValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JwtValidationError::Invalid => write!(f, "Invalid token"),
            JwtValidationError::Expired => write!(f, "Expired token"),
            JwtValidationError::KidMismatch => write!(f, "Token kid does not match"),
        }
    }
}

impl std::error::Error for JwtValidationError {}
