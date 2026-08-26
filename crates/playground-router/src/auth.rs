// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

#![cfg(not(target_family = "wasm"))]

//! GitHub OAuth (web application flow) for gating the playground.
//!
//! Routes handled (under the playground path):
//! - `__auth/login`: sets a short-lived CSRF state cookie and redirects to
//!   GitHub's authorize URL. No `redirect_uri` is passed, so GitHub uses the
//!   callback URL registered on the OAuth app — which must be
//!   `https://<host><playground-path>/__auth/callback`.
//! - `__auth/callback`: verifies the state, exchanges the code for a user
//!   token, checks the login/org allowlist, and issues the session cookie.

use common::context::RequestContext;
use common::http::{Headers, RequestPayload, ResponseBody, ResponsePayload};
use common::playground_auth::{
    PLAYGROUND_OAUTH_STATE_COOKIE, PLAYGROUND_SESSION_COOKIE, PlaygroundAuthConfig, cookie_value,
    issue_session_token, issue_state_token, validate_state_token,
};
use http::StatusCode;
use serde::Deserialize;

pub const AUTH_ROUTE_PREFIX: &str = "__auth/";

const GITHUB_AUTHORIZE_URL: &str = "https://github.com/login/oauth/authorize";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = "exograph-playground-auth";

/// Handle a `__auth/...` route. `sub_path` is the path after the playground
/// prefix (starts with `__auth/`).
pub async fn route_auth(
    request_context: &RequestContext<'_>,
    config: &PlaygroundAuthConfig,
    playground_path: &str,
    sub_path: &str,
) -> ResponsePayload {
    match sub_path.strip_prefix(AUTH_ROUTE_PREFIX) {
        Some("login") => login(config),
        Some("callback") => callback(request_context, config, playground_path).await,
        _ => plain_response(StatusCode::NOT_FOUND, "Not found"),
    }
}

pub fn login_redirect(playground_path: &str) -> ResponsePayload {
    redirect(&format!("/{playground_path}/__auth/login"), Headers::new())
}

fn login(config: &PlaygroundAuthConfig) -> ResponsePayload {
    let state = issue_state_token(&config.session_secret);

    // `read:org` is needed only to verify (possibly private) org membership.
    let scope = if config.allowed_github_org.is_some() {
        "read:org"
    } else {
        ""
    };

    let authorize_url = format!(
        "{GITHUB_AUTHORIZE_URL}?client_id={client_id}&scope={scope}&state={state}",
        client_id = config.github_client_id,
    );

    let mut headers = Headers::new();
    headers.insert(
        "set-cookie".to_string(),
        format!(
            "{PLAYGROUND_OAUTH_STATE_COOKIE}={state}; Max-Age=600; Path=/; HttpOnly; Secure; SameSite=Lax"
        ),
    );
    redirect(&authorize_url, headers)
}

async fn callback(
    request_context: &RequestContext<'_>,
    config: &PlaygroundAuthConfig,
    playground_path: &str,
) -> ResponsePayload {
    let head = request_context.get_head();
    let query = head.get_query();

    let Some(code) = query.get("code").and_then(|v| v.as_str()) else {
        return plain_response(StatusCode::BAD_REQUEST, "Missing 'code' parameter");
    };
    let Some(state) = query.get("state").and_then(|v| v.as_str()) else {
        return plain_response(StatusCode::BAD_REQUEST, "Missing 'state' parameter");
    };

    // CSRF check: the state must be one we minted (signature + expiry) and must
    // match the cookie set when the flow started.
    let state_cookie = head
        .get_headers("cookie")
        .iter()
        .find_map(|header| cookie_value(header, PLAYGROUND_OAUTH_STATE_COOKIE));
    if state_cookie.as_deref() != Some(state)
        || !validate_state_token(state, &config.session_secret)
    {
        return plain_response(
            StatusCode::FORBIDDEN,
            "OAuth state mismatch; retry signing in",
        );
    }

    let login = match github_signin(config, code).await {
        Ok(GithubSignin {
            login,
            allowed: true,
        }) => login,
        Ok(GithubSignin {
            login,
            allowed: false,
        }) => {
            tracing::warn!("Playground sign-in rejected for GitHub user '{login}'");
            return plain_response(
                StatusCode::FORBIDDEN,
                "Your GitHub account is not authorized to view this playground",
            );
        }
        Err(error) => {
            tracing::error!("Playground GitHub sign-in failed: {error}");
            return plain_response(
                StatusCode::BAD_GATEWAY,
                "Sign-in with GitHub failed; retry or check the server logs",
            );
        }
    };

    let session = issue_session_token(&login, &config.session_secret);

    let mut headers = Headers::new();
    // Path=/ so the cookie also accompanies introspection requests to the
    // GraphQL endpoint (which is gated on the same session).
    headers.insert(
        "set-cookie".to_string(),
        format!(
            "{PLAYGROUND_SESSION_COOKIE}={session}; Max-Age=86400; Path=/; HttpOnly; Secure; SameSite=Lax"
        ),
    );
    headers.insert(
        "set-cookie".to_string(),
        format!(
            "{PLAYGROUND_OAUTH_STATE_COOKIE}=; Max-Age=0; Path=/; HttpOnly; Secure; SameSite=Lax"
        ),
    );
    redirect(&format!("/{playground_path}"), headers)
}

struct GithubSignin {
    login: String,
    allowed: bool,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct UserResponse {
    login: String,
}

#[derive(Deserialize)]
struct OrgMembershipResponse {
    state: String,
}

async fn github_signin(config: &PlaygroundAuthConfig, code: &str) -> Result<GithubSignin, String> {
    let client = reqwest::Client::new();

    let token_response: TokenResponse = client
        .post(GITHUB_TOKEN_URL)
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .form(&[
            ("client_id", config.github_client_id.as_str()),
            ("client_secret", config.github_client_secret.as_str()),
            ("code", code),
        ])
        .send()
        .await
        .map_err(|e| format!("token exchange request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("token exchange returned invalid JSON: {e}"))?;

    let access_token = token_response.access_token.ok_or_else(|| {
        format!(
            "token exchange rejected: {}",
            token_response
                .error_description
                .unwrap_or_else(|| "no error description".to_string())
        )
    })?;

    let user: UserResponse = client
        .get(format!("{GITHUB_API_BASE}/user"))
        .bearer_auth(&access_token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| format!("user lookup failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("user lookup returned invalid JSON: {e}"))?;

    let is_org_member = match &config.allowed_github_org {
        Some(org) => {
            let response = client
                .get(format!("{GITHUB_API_BASE}/user/memberships/orgs/{org}"))
                .bearer_auth(&access_token)
                .header("Accept", "application/vnd.github+json")
                .header("User-Agent", USER_AGENT)
                .send()
                .await
                .map_err(|e| format!("org membership lookup failed: {e}"))?;

            response.status().is_success()
                && response
                    .json::<OrgMembershipResponse>()
                    .await
                    .map(|m| m.state == "active")
                    .unwrap_or(false)
        }
        None => false,
    };

    let allowed = config.is_login_allowed(&user.login, is_org_member);
    Ok(GithubSignin {
        login: user.login,
        allowed,
    })
}

fn redirect(location: &str, headers: Headers) -> ResponsePayload {
    ResponsePayload {
        body: ResponseBody::Redirect(location.to_string()),
        headers,
        status_code: StatusCode::FOUND,
    }
}

fn plain_response(status_code: StatusCode, message: &str) -> ResponsePayload {
    ResponsePayload {
        body: ResponseBody::Bytes(message.as_bytes().to_vec()),
        headers: Headers::from_vec(vec![(
            http::header::CONTENT_TYPE.to_string(),
            "text/plain; charset=utf-8".to_string(),
        )]),
        status_code,
    }
}
