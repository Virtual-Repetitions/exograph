// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Authentication for the MCP endpoint.
//!
//! The MCP endpoint needs a gate of its own. Its tools expose the whole
//! GraphQL schema — in the default `combined` tool mode a single unauthenticated
//! `tools/list` returns the full SDL, because the schema is embedded in the
//! `execute_query` tool's description — and `EXO_INTROSPECTION=false` does not
//! disable it, since the MCP router builds its own introspection resolver so
//! that MCP keeps working in production.
//!
//! (Data itself is not exposed by this: `execute_query` resolves through the
//! normal GraphQL pipeline, so `@access` rules apply against whatever JWT the
//! request carries. The gate here protects the endpoint and the schema.)
//!
//! A request is accepted when **either**:
//!
//! - it carries the shared secret in `X-Exo-MCP-Secret` and `EXO_MCP_SECRET` is
//!   set — intended for programmatic clients (`exo-mcp-bridge --header ...`,
//!   Claude Desktop's HTTP transport config); or
//! - it carries a valid playground OAuth session cookie and that gate is
//!   configured — so a human signed in through the playground can use its MCP
//!   tab, whose requests are same-origin and send the cookie automatically.
//!
//! The gate is enforced as soon as *either* mechanism is configured. That means
//! turning on the playground's GitHub gate also closes MCP, rather than leaving
//! it as a way around the very gate that was just enabled.
//!
//! A deliberately open deployment (neither configured) keeps the previous
//! behavior of serving MCP to anyone who can reach it.

use exo_env::Environment;

use crate::env_const::EXO_MCP_SECRET;
use crate::http::RequestHead;
use crate::playground_auth::{PlaygroundAuthConfig, request_session};

pub const MCP_SECRET_HEADER: &str = "X-Exo-MCP-Secret";

/// Rejects obviously guessable secrets. A generated 32+ character secret is
/// recommended (`openssl rand -base64 32`).
const MIN_SECRET_LEN: usize = 16;

pub struct McpAuthConfig {
    secret: Option<String>,
    playground_auth: Option<PlaygroundAuthConfig>,
}

impl McpAuthConfig {
    /// `Err` on invalid configuration; callers must fail closed rather than
    /// fall back to serving openly.
    pub fn from_env(env: &dyn Environment) -> Result<Self, String> {
        let secret = match env.get(EXO_MCP_SECRET) {
            Some(secret) => {
                let secret = secret.trim().to_string();
                // Set-but-empty means someone intended a gate and got none.
                if secret.is_empty() {
                    return Err(format!("{EXO_MCP_SECRET} is set but empty"));
                }
                if secret.len() < MIN_SECRET_LEN {
                    return Err(format!(
                        "{EXO_MCP_SECRET} must be at least {MIN_SECRET_LEN} characters"
                    ));
                }
                Some(secret)
            }
            None => None,
        };

        Ok(Self {
            secret,
            playground_auth: PlaygroundAuthConfig::from_env(env)?,
        })
    }

    /// Whether any credential is required to reach the MCP endpoint.
    pub fn is_enforced(&self) -> bool {
        self.secret.is_some() || self.playground_auth.is_some()
    }

    pub fn authorize(&self, head: &(dyn RequestHead + Sync)) -> bool {
        if !self.is_enforced() {
            return true;
        }

        if let Some(secret) = &self.secret
            && head
                .get_headers(MCP_SECRET_HEADER)
                .iter()
                .any(|presented| constant_time_eq(presented.trim(), secret))
        {
            return true;
        }

        // Fall back to a playground session so the playground's own MCP tab and
        // signed-in humans keep working without knowing the shared secret.
        match &self.playground_auth {
            Some(playground_auth) => request_session(head, playground_auth).is_some(),
            None => false,
        }
    }
}

/// Compares without leaking the position of the first difference through timing.
/// Lengths are not secret (and differ in the common reject case anyway).
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_const::{
        EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID, EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_SECRET,
        EXO_PLAYGROUND_AUTH_GITHUB_USERS, EXO_PLAYGROUND_AUTH_SESSION_SECRET,
    };
    use crate::http::MemoryRequestHead;
    use crate::playground_auth::{PLAYGROUND_SESSION_COOKIE, issue_session_token};
    use exo_env::MapEnvironment;
    use std::collections::HashMap;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn head_with(headers: Vec<(&str, &str)>) -> MemoryRequestHead {
        let mut head = MemoryRequestHead::new(
            HashMap::new(),
            HashMap::new(),
            http::Method::POST,
            "/mcp".to_string(),
            serde_json::Value::Null,
            None,
        );
        for (key, value) in headers {
            head.add_header(key, value);
        }
        head
    }

    fn playground_env() -> HashMap<String, String> {
        [
            (EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID, "id"),
            (EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_SECRET, "client-secret"),
            (EXO_PLAYGROUND_AUTH_SESSION_SECRET, SECRET),
            (EXO_PLAYGROUND_AUTH_GITHUB_USERS, "octocat"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn open_when_nothing_configured() {
        let config = McpAuthConfig::from_env(&MapEnvironment::from([])).unwrap();
        assert!(!config.is_enforced());
        assert!(config.authorize(&head_with(vec![])));
    }

    #[test]
    fn secret_required_when_configured() {
        let config =
            McpAuthConfig::from_env(&MapEnvironment::from([(EXO_MCP_SECRET, SECRET)])).unwrap();
        assert!(config.is_enforced());

        assert!(!config.authorize(&head_with(vec![])));
        assert!(!config.authorize(&head_with(vec![(MCP_SECRET_HEADER, "wrong-secret-wrong")])));
        assert!(config.authorize(&head_with(vec![(MCP_SECRET_HEADER, SECRET)])));
    }

    #[test]
    fn secret_header_is_case_insensitive() {
        let config =
            McpAuthConfig::from_env(&MapEnvironment::from([(EXO_MCP_SECRET, SECRET)])).unwrap();
        assert!(config.authorize(&head_with(vec![("x-exo-mcp-secret", SECRET)])));
    }

    #[test]
    fn weak_or_empty_secret_is_rejected() {
        assert!(McpAuthConfig::from_env(&MapEnvironment::from([(EXO_MCP_SECRET, "")])).is_err());
        assert!(
            McpAuthConfig::from_env(&MapEnvironment::from([(EXO_MCP_SECRET, "short")])).is_err()
        );
    }

    #[test]
    fn playground_gate_alone_closes_mcp() {
        let config = McpAuthConfig::from_env(&MapEnvironment::from(playground_env())).unwrap();
        assert!(config.is_enforced());
        assert!(!config.authorize(&head_with(vec![])));

        let session = issue_session_token("octocat", SECRET);
        assert!(config.authorize(&head_with(vec![(
            "cookie",
            &format!("{PLAYGROUND_SESSION_COOKIE}={session}"),
        )])));
    }

    #[test]
    fn session_cookie_accepted_alongside_secret() {
        let mut env = playground_env();
        env.insert(EXO_MCP_SECRET.to_string(), SECRET.to_string());
        let config = McpAuthConfig::from_env(&MapEnvironment::from(env)).unwrap();

        let session = issue_session_token("octocat", SECRET);
        assert!(config.authorize(&head_with(vec![(
            "cookie",
            &format!("{PLAYGROUND_SESSION_COOKIE}={session}"),
        )])));
        assert!(config.authorize(&head_with(vec![(MCP_SECRET_HEADER, SECRET)])));
    }

    #[test]
    fn forged_session_rejected() {
        let config = McpAuthConfig::from_env(&MapEnvironment::from(playground_env())).unwrap();
        let forged = issue_session_token("octocat", "another-secret-another-secret!!!");
        assert!(!config.authorize(&head_with(vec![(
            "cookie",
            &format!("{PLAYGROUND_SESSION_COOKIE}={forged}"),
        )])));
    }

    #[test]
    fn misconfigured_playground_auth_propagates() {
        // Client id without the rest must not silently leave MCP open.
        let env = MapEnvironment::from([(EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID, "id")]);
        assert!(McpAuthConfig::from_env(&env).is_err());
    }

    #[test]
    fn constant_time_eq_matches_normal_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(constant_time_eq("", ""));
    }
}
