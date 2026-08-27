// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Shared machinery for endpoint gates that accept either a shared secret in a
//! dedicated header or a valid playground OAuth session cookie.
//!
//! Both the MCP gate ([`crate::mcp_auth`]) and the introspection gate
//! ([`crate::introspection_auth`]) follow the same rules:
//!
//! - A request is accepted when it carries the configured shared secret in the
//!   gate's header (intended for programmatic clients), **or** a valid
//!   playground session cookie while that gate is configured (so a human signed
//!   in through the playground keeps working — its requests are same-origin and
//!   send the cookie automatically).
//! - The gate is enforced as soon as *either* mechanism is configured.
//! - A deliberately open deployment (neither configured) admits everyone.
//! - Misconfiguration (empty or weak secret, partial playground config) is an
//!   `Err`; callers must fail closed rather than fall back to serving openly.

use exo_env::Environment;

use crate::http::RequestHead;
use crate::playground_auth::{PlaygroundAuthConfig, request_session};

/// Rejects obviously guessable secrets. A generated 32+ character secret is
/// recommended (`openssl rand -base64 32`).
const MIN_SECRET_LEN: usize = 16;

pub struct EndpointGate {
    secret_header: &'static str,
    secret: Option<String>,
    playground_auth: Option<PlaygroundAuthConfig>,
}

impl EndpointGate {
    /// Reads `secret_var` and the playground auth configuration from `env`.
    /// `Err` on invalid configuration; callers must fail closed rather than
    /// fall back to serving openly.
    pub fn from_env(
        env: &dyn Environment,
        secret_var: &str,
        secret_header: &'static str,
    ) -> Result<Self, String> {
        let secret = match env.get(secret_var) {
            Some(secret) => {
                let secret = secret.trim().to_string();
                // Set-but-empty means someone intended a gate and got none.
                if secret.is_empty() {
                    return Err(format!("{secret_var} is set but empty"));
                }
                if secret.len() < MIN_SECRET_LEN {
                    return Err(format!(
                        "{secret_var} must be at least {MIN_SECRET_LEN} characters"
                    ));
                }
                Some(secret)
            }
            None => None,
        };

        Ok(Self {
            secret_header,
            secret,
            playground_auth: PlaygroundAuthConfig::from_env(env)?,
        })
    }

    /// Whether any credential is required to pass the gate.
    pub fn is_enforced(&self) -> bool {
        self.secret.is_some() || self.playground_auth.is_some()
    }

    pub fn authorize(&self, head: &(dyn RequestHead + Sync)) -> bool {
        if !self.is_enforced() {
            return true;
        }

        if let Some(secret) = &self.secret
            && head
                .get_headers(self.secret_header)
                .iter()
                .any(|presented| constant_time_eq(presented.trim(), secret))
        {
            return true;
        }

        // Fall back to a playground session so signed-in humans keep working
        // without knowing the shared secret.
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

    #[test]
    fn constant_time_eq_matches_normal_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(constant_time_eq("", ""));
    }
}
