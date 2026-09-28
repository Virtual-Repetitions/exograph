use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde_json::Value;
use tracing::{debug, error, info};

use super::authenticator::JwtConfigurationError;
use super::jwks::JwtValidationError;

pub struct StaticKeyValidator {
    name: String,
    kid: Option<String>,
    decoding_key: DecodingKey,
    algorithm: Algorithm,
    allowed_audiences: Option<Vec<String>>,
}

impl StaticKeyValidator {
    pub fn from_pem(
        name: impl Into<String>,
        pem: String,
        kid: Option<String>,
        allowed_audiences: Option<Vec<String>>,
    ) -> Result<Self, JwtConfigurationError> {
        let name = name.into();
        let normalized_pem = pem.replace("\\n", "\n");
        // Accept either an RSA (RS256) or an Ed25519 (EdDSA) public key.
        let (decoding_key, algorithm) = match DecodingKey::from_rsa_pem(normalized_pem.as_bytes()) {
            Ok(key) => (key, Algorithm::RS256),
            Err(rsa_err) => match DecodingKey::from_ed_pem(normalized_pem.as_bytes()) {
                Ok(key) => (key, Algorithm::EdDSA),
                Err(ed_err) => {
                    return Err(JwtConfigurationError::Configuration {
                        message: format!(
                            "Invalid public key in '{}': not RSA ({}) nor Ed25519 ({})",
                            name, rsa_err, ed_err
                        ),
                        source: Box::new(rsa_err),
                    });
                }
            },
        };

        if let Some(kid) = &kid {
            info!(
                "Configured static JWT public key '{}' ({:?}) with kid '{}'",
                name, algorithm, kid
            );
        } else {
            info!(
                "Configured static JWT public key '{}' ({:?})",
                name, algorithm
            );
        }

        Ok(Self {
            name,
            kid,
            decoding_key,
            algorithm,
            allowed_audiences,
        })
    }

    pub fn validate(&self, token: &str) -> Result<Value, JwtValidationError> {
        let header = decode_header(token).map_err(|err| {
            error!(
                "Failed to decode JWT header for static key '{}': {}",
                self.name, err
            );
            JwtValidationError::Invalid
        })?;

        if let Some(expected_kid) = &self.kid {
            match header.kid.as_deref() {
                Some(actual_kid) if actual_kid == expected_kid => {}
                Some(_) | None => {
                    debug!(
                        "Skipping static key '{}' due to kid mismatch (expected '{}', got '{:?}')",
                        self.name, expected_kid, header.kid
                    );
                    return Err(JwtValidationError::KidMismatch);
                }
            }
        }

        let mut validation = Validation::new(self.algorithm);
        validation.validate_exp = true;
        validation.validate_nbf = false;

        if let Some(audiences) = &self.allowed_audiences {
            validation.set_audience(audiences);
            validation.validate_aud = true;
        } else {
            validation.validate_aud = false;
        }

        validation.set_required_spec_claims::<&str>(&[]);

        decode::<Value>(token, &self.decoding_key, &validation)
            .map(|data| data.claims)
            .map_err(|err| match err.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                    error!(
                        "JWT validation failed for static key '{}': expired signature",
                        self.name
                    );
                    JwtValidationError::Expired
                }
                _ => {
                    debug!(
                        "JWT validation with static key '{}' failed: {}",
                        self.name, err
                    );
                    JwtValidationError::Invalid
                }
            })
    }
}

impl StaticKeyValidator {
    pub fn debug_name(&self) -> &str {
        &self.name
    }

    pub fn debug_kid(&self) -> Option<&str> {
        self.kid.as_deref()
    }

    pub fn debug_allowed_audiences(&self) -> Option<&[String]> {
        self.allowed_audiences.as_deref()
    }
}
