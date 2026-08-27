// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Authentication for introspection queries.
//!
//! The playground's GitHub-OAuth gate holds introspection to the same session
//! as the playground HTML — a hosted playground needs introspection enabled,
//! and gating only the HTML would leave the schema publicly fetchable. But a
//! session cookie can only be obtained by a human completing the OAuth flow in
//! a browser, which locks out headless introspection clients entirely:
//! codegen (e.g. AutoGQL's schema download), schema diffing, and CI validation
//! all see a blanket `Not authorized`.
//!
//! This gate mirrors the MCP endpoint's ([`crate::mcp_auth`]): a request is
//! accepted when **either**:
//!
//! - it carries the shared secret in `X-Exo-Introspection-Secret` and
//!   `EXO_INTROSPECTION_SECRET` is set — intended for programmatic clients; or
//! - it carries a valid playground OAuth session cookie and that gate is
//!   configured — so the hosted playground keeps working for signed-in humans.
//!
//! The gate is enforced as soon as *either* mechanism is configured, so the
//! secret can also gate introspection on a deployment that does not run the
//! playground gate at all. With neither configured, introspection exposure is
//! governed by `EXO_INTROSPECTION` alone, as before.
//!
//! (Data access is unchanged in every case: this guards `__schema`/`__type`,
//! not data operations, which remain governed by the model's `@access` rules.)

use exo_env::Environment;

use crate::endpoint_gate::EndpointGate;
use crate::env_const::EXO_INTROSPECTION_SECRET;
use crate::http::RequestHead;

pub const INTROSPECTION_SECRET_HEADER: &str = "X-Exo-Introspection-Secret";

pub struct IntrospectionAuthConfig {
    gate: EndpointGate,
}

impl IntrospectionAuthConfig {
    /// `Err` on invalid configuration; callers must fail closed rather than
    /// fall back to serving openly.
    pub fn from_env(env: &dyn Environment) -> Result<Self, String> {
        Ok(Self {
            gate: EndpointGate::from_env(
                env,
                EXO_INTROSPECTION_SECRET,
                INTROSPECTION_SECRET_HEADER,
            )?,
        })
    }

    /// Whether any credential is required to run introspection queries.
    pub fn is_enforced(&self) -> bool {
        self.gate.is_enforced()
    }

    pub fn authorize(&self, head: &(dyn RequestHead + Sync)) -> bool {
        self.gate.authorize(head)
    }
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
            "/graphql".to_string(),
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
        let config = IntrospectionAuthConfig::from_env(&MapEnvironment::from([])).unwrap();
        assert!(!config.is_enforced());
        assert!(config.authorize(&head_with(vec![])));
    }

    #[test]
    fn secret_alone_gates_introspection() {
        let config = IntrospectionAuthConfig::from_env(&MapEnvironment::from([(
            EXO_INTROSPECTION_SECRET,
            SECRET,
        )]))
        .unwrap();
        assert!(config.is_enforced());

        assert!(!config.authorize(&head_with(vec![])));
        assert!(!config.authorize(&head_with(vec![(
            INTROSPECTION_SECRET_HEADER,
            "wrong-secret-wrong"
        )])));
        assert!(config.authorize(&head_with(vec![(INTROSPECTION_SECRET_HEADER, SECRET)])));
    }

    #[test]
    fn secret_header_is_case_insensitive() {
        let config = IntrospectionAuthConfig::from_env(&MapEnvironment::from([(
            EXO_INTROSPECTION_SECRET,
            SECRET,
        )]))
        .unwrap();
        assert!(config.authorize(&head_with(vec![("x-exo-introspection-secret", SECRET)])));
    }

    #[test]
    fn weak_or_empty_secret_is_rejected() {
        assert!(
            IntrospectionAuthConfig::from_env(&MapEnvironment::from([(
                EXO_INTROSPECTION_SECRET,
                ""
            )]))
            .is_err()
        );
        assert!(
            IntrospectionAuthConfig::from_env(&MapEnvironment::from([(
                EXO_INTROSPECTION_SECRET,
                "short"
            )]))
            .is_err()
        );
    }

    #[test]
    fn playground_session_still_accepted() {
        let config =
            IntrospectionAuthConfig::from_env(&MapEnvironment::from(playground_env())).unwrap();
        assert!(config.is_enforced());
        assert!(!config.authorize(&head_with(vec![])));

        let session = issue_session_token("octocat", SECRET);
        assert!(config.authorize(&head_with(vec![(
            "cookie",
            &format!("{PLAYGROUND_SESSION_COOKIE}={session}"),
        )])));
    }

    #[test]
    fn secret_accepted_alongside_playground_gate() {
        let mut env = playground_env();
        env.insert(EXO_INTROSPECTION_SECRET.to_string(), SECRET.to_string());
        let config = IntrospectionAuthConfig::from_env(&MapEnvironment::from(env)).unwrap();

        assert!(config.authorize(&head_with(vec![(INTROSPECTION_SECRET_HEADER, SECRET)])));

        let session = issue_session_token("octocat", SECRET);
        assert!(config.authorize(&head_with(vec![(
            "cookie",
            &format!("{PLAYGROUND_SESSION_COOKIE}={session}"),
        )])));
    }

    #[test]
    fn forged_session_rejected() {
        let config =
            IntrospectionAuthConfig::from_env(&MapEnvironment::from(playground_env())).unwrap();
        let forged = issue_session_token("octocat", "another-secret-another-secret!!!");
        assert!(!config.authorize(&head_with(vec![(
            "cookie",
            &format!("{PLAYGROUND_SESSION_COOKIE}={forged}"),
        )])));
    }

    #[test]
    fn misconfigured_playground_auth_propagates() {
        // Client id without the rest must not silently leave introspection open.
        let env = MapEnvironment::from([(EXO_PLAYGROUND_AUTH_GITHUB_CLIENT_ID, "id")]);
        assert!(IntrospectionAuthConfig::from_env(&env).is_err());
    }
}
