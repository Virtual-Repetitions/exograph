// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! GitHub-OAuth gate for the playground.
//!
//! When `EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID` is set, the playground assets
//! are served only to requests carrying a valid session cookie, and (since a
//! hosted playground is useless without introspection) introspection queries
//! are held to the same session. The OAuth flow itself lives in
//! `playground-router`; this module holds what both sides of the gate need:
//! the configuration and the session/state token handling.

use std::time::{SystemTime, UNIX_EPOCH};

use exo_env::Environment;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::env_const::{
    EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID, EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_SECRET,
    EXO_PLAYGROUND_AUTH_GITHUB_ORG, EXO_PLAYGROUND_AUTH_GITHUB_USERS,
    EXO_PLAYGROUND_AUTH_SESSION_SECRET,
};
use crate::http::RequestHead;

pub const PLAYGROUND_SESSION_COOKIE: &str = "exo_playground_session";
pub const PLAYGROUND_OAUTH_STATE_COOKIE: &str = "exo_playground_oauth_state";

const SESSION_AUDIENCE: &str = "exo-playground-session";
const STATE_AUDIENCE: &str = "exo-playground-oauth-state";

const SESSION_TTL_SECONDS: u64 = 24 * 60 * 60;
const STATE_TTL_SECONDS: u64 = 10 * 60;

#[derive(Clone)]
pub struct PlaygroundAuthConfig {
    pub github_client_id: String,
    pub github_client_secret: String,
    /// Members of this GitHub org are allowed (checked via the user's own token,
    /// so private membership works when the `read:org` scope is granted).
    pub allowed_github_org: Option<String>,
    /// These GitHub logins are allowed regardless of org membership (lowercased).
    pub allowed_github_users: Vec<String>,
    pub session_secret: String,
}

impl PlaygroundAuthConfig {
    /// Returns `Ok(None)` when the gate is not configured (client id unset).
    /// Returns `Err` when it is partially or invalidly configured — the caller
    /// must fail closed in that case, not fall back to serving openly.
    pub fn from_env(env: &dyn Environment) -> Result<Option<Self>, String> {
        let Some(github_client_id) = non_empty(env.get(EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID))
        else {
            return Ok(None);
        };

        let github_client_secret = non_empty(env.get(EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_SECRET))
            .ok_or_else(|| format!("{EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_SECRET} must be set"))?;

        let session_secret = non_empty(env.get(EXO_PLAYGROUND_AUTH_SESSION_SECRET))
            .ok_or_else(|| format!("{EXO_PLAYGROUND_AUTH_SESSION_SECRET} must be set"))?;
        if session_secret.len() < 32 {
            return Err(format!(
                "{EXO_PLAYGROUND_AUTH_SESSION_SECRET} must be at least 32 characters"
            ));
        }

        let allowed_github_org = non_empty(env.get(EXO_PLAYGROUND_AUTH_GITHUB_ORG));
        let allowed_github_users: Vec<String> = env
            .get(EXO_PLAYGROUND_AUTH_GITHUB_USERS)
            .map(|users| {
                users
                    .split(',')
                    .map(|u| u.trim().to_lowercase())
                    .filter(|u| !u.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        if allowed_github_org.is_none() && allowed_github_users.is_empty() {
            return Err(format!(
                "At least one of {EXO_PLAYGROUND_AUTH_GITHUB_ORG} or {EXO_PLAYGROUND_AUTH_GITHUB_USERS} must be set"
            ));
        }

        Ok(Some(Self {
            github_client_id,
            github_client_secret,
            allowed_github_org,
            allowed_github_users,
            session_secret,
        }))
    }

    pub fn is_login_allowed(&self, login: &str, is_org_member: bool) -> bool {
        self.allowed_github_users.contains(&login.to_lowercase())
            || (self.allowed_github_org.is_some() && is_org_member)
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[derive(Serialize, Deserialize)]
struct TokenClaims {
    sub: String,
    aud: String,
    iat: u64,
    exp: u64,
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn issue_token(sub: &str, audience: &str, ttl_seconds: u64, secret: &str) -> String {
    let now = now_epoch_seconds();
    let claims = TokenClaims {
        sub: sub.to_string(),
        aud: audience.to_string(),
        iat: now,
        exp: now + ttl_seconds,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("HS256 signing of playground session token cannot fail")
}

fn validate_token(token: &str, audience: &str, secret: &str) -> Option<String> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[audience]);
    decode::<TokenClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()
    .map(|data| data.claims.sub)
}

/// Session cookie value for a signed-in GitHub user (24h expiry).
pub fn issue_session_token(github_login: &str, secret: &str) -> String {
    issue_token(github_login, SESSION_AUDIENCE, SESSION_TTL_SECONDS, secret)
}

/// Returns the GitHub login when the token is a valid, unexpired session.
pub fn validate_session_token(token: &str, secret: &str) -> Option<String> {
    validate_token(token, SESSION_AUDIENCE, secret)
}

/// Short-lived CSRF state for the OAuth roundtrip (also stored in a cookie).
pub fn issue_state_token(secret: &str) -> String {
    issue_token("state", STATE_AUDIENCE, STATE_TTL_SECONDS, secret)
}

pub fn validate_state_token(token: &str, secret: &str) -> bool {
    validate_token(token, STATE_AUDIENCE, secret).is_some()
}

/// Extract a cookie's value from a `Cookie:` request header.
pub fn cookie_value(cookie_header: &str, name: &str) -> Option<String> {
    cookie_header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_string())
    })
}

/// Returns the GitHub login when the request carries a valid session cookie.
pub fn request_session(
    head: &(dyn RequestHead + Sync),
    config: &PlaygroundAuthConfig,
) -> Option<String> {
    head.get_headers("cookie").iter().find_map(|header| {
        let token = cookie_value(header, PLAYGROUND_SESSION_COOKIE)?;
        validate_session_token(&token, &config.session_secret)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use exo_env::MapEnvironment;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn session_roundtrip() {
        let token = issue_session_token("octocat", SECRET);
        assert_eq!(
            validate_session_token(&token, SECRET),
            Some("octocat".to_string())
        );
    }

    #[test]
    fn session_rejects_wrong_secret() {
        let token = issue_session_token("octocat", SECRET);
        assert_eq!(
            validate_session_token(&token, "another-secret-another-secret!!!"),
            None
        );
    }

    #[test]
    fn state_token_is_not_a_session() {
        let token = issue_state_token(SECRET);
        assert_eq!(validate_session_token(&token, SECRET), None);
        assert!(validate_state_token(&token, SECRET));
    }

    #[test]
    fn cookie_parsing() {
        let header = "a=1; exo_playground_session=tok en ; b=2";
        assert_eq!(
            cookie_value(header, PLAYGROUND_SESSION_COOKIE),
            Some("tok en".to_string())
        );
        assert_eq!(cookie_value(header, "missing"), None);
    }

    #[test]
    fn config_absent_when_client_id_unset() {
        let env = MapEnvironment::from([]);
        assert!(PlaygroundAuthConfig::from_env(&env).unwrap().is_none());
    }

    #[test]
    fn config_fails_closed_when_partial() {
        let env =
            MapEnvironment::from([(crate::env_const::EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID, "id")]);
        assert!(PlaygroundAuthConfig::from_env(&env).is_err());
    }

    #[test]
    fn config_requires_org_or_users() {
        let env = MapEnvironment::from([
            (crate::env_const::EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID, "id"),
            (
                crate::env_const::EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_SECRET,
                "secret",
            ),
            (crate::env_const::EXO_PLAYGROUND_AUTH_SESSION_SECRET, SECRET),
        ]);
        assert!(PlaygroundAuthConfig::from_env(&env).is_err());

        let env = MapEnvironment::from([
            (crate::env_const::EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID, "id"),
            (
                crate::env_const::EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_SECRET,
                "secret",
            ),
            (crate::env_const::EXO_PLAYGROUND_AUTH_SESSION_SECRET, SECRET),
            (
                crate::env_const::EXO_PLAYGROUND_AUTH_GITHUB_USERS,
                "Alice, bob",
            ),
        ]);
        let config = PlaygroundAuthConfig::from_env(&env).unwrap().unwrap();
        assert!(config.is_login_allowed("ALICE", false));
        assert!(config.is_login_allowed("bob", false));
        assert!(!config.is_login_allowed("mallory", false));
    }

    #[test]
    fn org_membership_allows() {
        let env = MapEnvironment::from([
            (crate::env_const::EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID, "id"),
            (
                crate::env_const::EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_SECRET,
                "secret",
            ),
            (crate::env_const::EXO_PLAYGROUND_AUTH_SESSION_SECRET, SECRET),
            (crate::env_const::EXO_PLAYGROUND_AUTH_GITHUB_ORG, "acme"),
        ]);
        let config = PlaygroundAuthConfig::from_env(&env).unwrap().unwrap();
        assert!(config.is_login_allowed("anyone", true));
        assert!(!config.is_login_allowed("anyone", false));
    }
}
