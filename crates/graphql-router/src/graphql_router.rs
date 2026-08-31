// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::sync::Arc;

use async_trait::async_trait;
use common::env_const::get_graphql_http_path;

use common::env_const::is_production;
use common::http::{Headers, RequestHead, RequestPayload, ResponseBody, ResponsePayload};
use common::router::Router;
use core_plugin_shared::interception::InterceptionMap;
use core_plugin_shared::trusted_documents::TrustedDocumentEnforcement;
use core_plugin_shared::trusted_documents::TrustedDocuments;
use core_resolver::QueryResponse;
use core_resolver::introspection::definition::schema::Schema;
use core_resolver::plugin::SubsystemGraphQLResolver;
use core_resolver::plugin::subsystem_graphql_resolver::SubsystemResolutionError;
use core_router::SystemLoadingError;
use http::StatusCode;
use sentry::Level;

use ::tracing::Instrument;
use ::tracing::instrument;
use async_graphql_parser::Pos;
use async_stream::try_stream;
use bytes::Bytes;
use common::context::RequestContext;
use common::operation_payload::OperationsPayload;
use core_resolver::QueryResponseBody;
use core_resolver::system_resolver::GraphQLSystemResolver;
use core_resolver::system_resolver::{RequestError, SystemResolutionError};

use exo_env::Environment;

use crate::system_loader::SystemLoader;

fn extract_request_id(request_head: &(dyn RequestHead + Sync)) -> Option<String> {
    request_head
        .get_header("x-request-id")
        .or_else(|| request_head.get_header("x-correlation-id"))
        .or_else(|| request_head.get_header("x-amzn-trace-id"))
        .or_else(|| request_head.get_header("traceparent"))
}

pub struct GraphQLRouter {
    resolver: Arc<GraphQLSystemResolver>,
    env: Arc<dyn Environment>,
}

impl GraphQLRouter {
    pub fn new(resolver: GraphQLSystemResolver, env: Arc<dyn Environment>) -> Self {
        Self {
            resolver: Arc::new(resolver),
            env,
        }
    }

    fn suitable(&self, request_head: &(dyn RequestHead + Sync)) -> bool {
        request_head.get_path() == get_graphql_http_path(self.env.as_ref())
            && request_head.get_method() == http::Method::POST
    }

    pub fn from_resolvers(
        graphql_resolvers: Vec<Arc<dyn SubsystemGraphQLResolver + Send + Sync>>,
        introspection_resolver: Option<Arc<dyn SubsystemGraphQLResolver + Send + Sync>>,
        schema: Arc<Schema>,
        query_interception_map: Arc<InterceptionMap>,
        mutation_interception_map: Arc<InterceptionMap>,
        trusted_documents: TrustedDocuments,
        env: Arc<dyn Environment>,
    ) -> Result<Self, SystemLoadingError> {
        let graphql_resolver = SystemLoader::create_system_resolver(
            graphql_resolvers,
            introspection_resolver,
            query_interception_map,
            mutation_interception_map,
            trusted_documents,
            env.clone(),
            schema,
        )?;

        Ok(Self::new(graphql_resolver, env))
    }

    pub fn resolver(&self) -> Arc<GraphQLSystemResolver> {
        self.resolver.clone()
    }
}

/// Whether this error is an ordinary, expected outcome rather than a fault the
/// server did not intend.
///
/// The distinction matters because every one of these used to be filed as a
/// production Sentry error at `Level::Error`. A client that sends a query the
/// published schema does not accept, or asks for something it is not allowed to
/// see, has not caused a server fault — the engine behaved exactly as designed
/// and told it so. Reporting those as errors buries genuine faults: the loudest
/// issues in the project were access denials and stale-client validation
/// failures, while real `Internal server error` events sat underneath them.
///
/// Every arm here is a message the engine deliberately renders for the caller.
/// Anything not listed is treated as a fault and still captured, so a new error
/// variant is reported by default rather than silently dropped.
fn is_expected_user_error(err: &SystemResolutionError) -> bool {
    match err {
        // The query does not match the published schema — a stale or hand-written
        // client. Nothing executed.
        SystemResolutionError::Validation(_) => true,
        // Malformed request: unparseable body, wrong content type, and similar.
        SystemResolutionError::RequestError(_) => true,
        // The document is not in the trusted-document allowlist. That is the
        // allowlist working.
        SystemResolutionError::TrustedDocumentResolution(_) => true,
        SystemResolutionError::SubsystemResolutionError(subsystem_err) => match subsystem_err {
            // Access rules denying a request is the access control working.
            SubsystemResolutionError::Authorization => true,
            // The caller selected a field that does not exist on the type.
            SubsystemResolutionError::InvalidField(_, _) => true,
            // A business rule a subsystem chose to surface (`ExographError`) —
            // "Invalid or expired code", "Gear already owned", and the like.
            SubsystemResolutionError::UserDisplayError(_) => true,
            // Context extraction failing can mean a malformed token, but it also
            // catches real misconfiguration (a null `@query` context field
            // silently denying every request), so keep reporting it.
            SubsystemResolutionError::ContextExtraction(_) => false,
            // Documented in the variant itself as almost certainly a programming
            // error.
            SubsystemResolutionError::NoInterceptorFound => false,
        },
        _ => false,
    }
}

/// Escape hatch: set `EXO_SENTRY_CAPTURE_EXPECTED_ERRORS=true` to restore the
/// previous behaviour of capturing everything. Useful when investigating an
/// environment where an expected-looking error is in fact the symptom.
fn capture_expected_errors() -> bool {
    std::env::var("EXO_SENTRY_CAPTURE_EXPECTED_ERRORS")
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            value == "true" || value == "1" || value == "yes"
        })
        .unwrap_or(false)
}

fn capture_graphql_error(
    err: &SystemResolutionError,
    request_context: &RequestContext<'_>,
    status_code: StatusCode,
    operation_name: Option<&str>,
) {
    if sentry::Hub::current().client().is_none() {
        return;
    }

    // Expected outcomes are still logged below, and still returned to the
    // caller unchanged — they are only kept out of the Sentry error stream.
    if is_expected_user_error(err) && !capture_expected_errors() {
        tracing::debug!(
            error.kind = %format!("{:?}", err),
            internal_request = request_context.is_internal(),
            "Expected GraphQL error, not captured to Sentry"
        );
        return;
    }

    let request_head = request_context.get_head();
    let message = err.user_error_message();

    sentry::with_scope(
        |scope| {
            scope.set_tag("graphql.path", request_head.get_path());
            scope.set_tag("http.method", request_head.get_method().as_str());
            if let Some(request_id) = extract_request_id(request_head) {
                scope.set_tag("request_id", request_id);
            }
            scope.set_tag("error.kind", format!("{:?}", err));
            // `graphql.path` is always `/graphql`, so without these an issue
            // cannot be attributed to an operation or a caller. Both come from
            // the request rather than the resolved document, so they survive a
            // validation failure — which is exactly the case that needed them.
            scope.set_tag(
                "graphql.operation",
                operation_name.unwrap_or("<unnamed>").to_string(),
            );
            if let Some(user_agent) = request_head.get_header("user-agent") {
                scope.set_tag("client.user_agent", user_agent);
            }
            scope.set_tag(
                "internal_request",
                request_context.is_internal().to_string(),
            );
            scope.set_tag(
                "auth_present",
                request_context.is_authentication_info_present().to_string(),
            );
            scope.set_extra("status_code", status_code.as_u16().into());
        },
        || {
            sentry::capture_message(&message, Level::Error);
        },
    );
}

#[async_trait]
impl<'a> Router<RequestContext<'a>> for GraphQLRouter {
    /// Resolves an incoming query, returning a response stream containing JSON and a set
    /// of HTTP headers. The JSON may be either the data returned by the query, or a list of errors
    /// if something went wrong.
    ///
    /// In a typical use case (for example server-actix), the caller will
    /// first call `create_system_resolver_or_exit` to create a [SystemResolver] object, and
    /// then call `resolve` with that object.
    #[instrument(
        name = "resolver::resolve"
        skip(self, request_context)
    )]
    async fn route(&self, request_context: &RequestContext<'a>) -> Option<ResponsePayload> {
        let request_head = request_context.get_head();
        if !self.suitable(request_head) {
            return None;
        }

        let request_id = extract_request_id(request_head);
        let request_span = tracing::info_span!(
            "graphql_request",
            request_id = request_id.as_deref().unwrap_or(""),
            method = %request_head.get_method(),
            path = %request_head.get_path(),
            client_ip = ?request_head.get_ip(),
            internal = request_context.is_internal()
        );
        // Instrument the resolution future with the span instead of holding an
        // `enter()` guard across `.await` points, which corrupts the subscriber's
        // span state (entered spans accumulate against whatever future the worker
        // polls next and exits mismatch).
        async {
        tracing::info!("GraphQL request received");

        let playground_request = request_head
            .get_header("_exo_playground")
            .map(|value| value == "true")
            .unwrap_or(false);

        let is_production = is_production(self.env.as_ref());
        let is_internal = request_context.is_internal();

        let should_enforce = !is_internal && (is_production || !playground_request);
        let trusted_document_enforcement = if should_enforce {
            TrustedDocumentEnforcement::Enforce
        } else {
            TrustedDocumentEnforcement::DoNotEnforce
        };

        // Inlined from `resolve_in_memory` so the operation name is still in
        // scope when an error is captured below. `take_body` yields Null on a
        // second call, so it cannot be recovered afterwards — and a validation
        // failure never produces a resolved document to read it from either.
        let body = request_context.take_body();

        let operations_payload = match OperationsPayload::from_json(body) {
            Ok(payload) => payload,
            Err(e) => {
                let err =
                    SystemResolutionError::RequestError(RequestError::InvalidBodyJson(e));
                tracing::error!("Error while resolving request: {:?}", err);
                capture_graphql_error(&err, request_context, StatusCode::BAD_REQUEST, None);
                return Some(ResponsePayload {
                    body: ResponseBody::None,
                    headers: Headers::new(),
                    status_code: StatusCode::BAD_REQUEST,
                });
            }
        };

        let operation_name = operations_payload.operation_name.clone();

        let response = resolve_in_memory_for_payload(
            operations_payload,
            &self.resolver,
            trusted_document_enforcement,
            request_context,
        )
        .await;

        match &response {
            Err(err @ SystemResolutionError::RequestError(e)) => {
                tracing::error!("Error while resolving request: {:?}", e);
                capture_graphql_error(
                    err,
                    request_context,
                    StatusCode::BAD_REQUEST,
                    operation_name.as_deref(),
                );
                return Some(ResponsePayload {
                    body: ResponseBody::None,
                    headers: Headers::new(),
                    status_code: StatusCode::BAD_REQUEST,
                });
            }
            Err(err) => {
                capture_graphql_error(
                    err,
                    request_context,
                    StatusCode::OK,
                    operation_name.as_deref(),
                );
            }
            Ok(_) => {}
        }

        let mut headers = if let Ok(ref response) = response {
            Headers::from_vec(
                response
                    .iter()
                    .flat_map(|(_, qr)| qr.headers.clone())
                    .collect(),
            )
        } else {
            Headers::new()
        };

        headers.insert("content-type".into(), "application/json".into());

        let stream = try_stream! {
            macro_rules! report_position {
                ($position:expr) => {
                    let p: Pos = $position;

                    yield Bytes::from_static(br#"{"line": "#);
                    yield Bytes::from(p.line.to_string());
                    yield Bytes::from_static(br#", "column": "#);
                    yield Bytes::from(p.column.to_string());
                    yield Bytes::from_static(br#"}"#);
                };
            }

            macro_rules! report_positions {
                ($positions:expr) => {
                    let mut first = true;
                    for p in $positions {
                        if !first {
                            yield Bytes::from_static(b", ");
                        }
                        first = false;
                        report_position!(p);
                    }
                };
            }

            match response {
                Ok(parts) => {
                    let parts_len = parts.len();
                    yield Bytes::from_static(br#"{"data": {"#);
                    for (index, part) in parts.into_iter().enumerate() {
                        yield Bytes::from_static(b"\"");
                        yield Bytes::from(part.0);
                        yield Bytes::from_static(br#"":"#);
                        match part.1.body {
                            QueryResponseBody::Json(value) => yield Bytes::from(value.to_string()),
                            QueryResponseBody::Raw(Some(value)) => yield Bytes::from(value),
                            QueryResponseBody::Raw(None) => yield Bytes::from_static(b"null"),
                        };
                        if index != parts_len - 1 {
                            yield Bytes::from_static(b", ");
                        }
                    };
                    yield Bytes::from_static(b"}}");
                },
                Err(err) => {
                    yield Bytes::from_static(br#"{"errors": [{"message":""#);
                    yield Bytes::from(
                        err.user_error_message().to_string()
                            .replace('\"', "")
                            .replace('\n', "; ")
                    );
                    yield Bytes::from_static(br#"""#);
                    if let SystemResolutionError::Validation(err) = err {
                        yield Bytes::from_static(br#", "locations": ["#);
                        report_positions!(err.positions());
                        yield Bytes::from_static(br#"]"#);
                    };
                    yield Bytes::from_static(br#"}"#);
                    yield Bytes::from_static(b"]}");
                },
            }
        };

        Some(ResponsePayload {
            body: ResponseBody::Stream(Box::pin(stream)),
            headers,
            status_code: StatusCode::OK,
        })
        }
        .instrument(request_span)
        .await
    }
}

// Carries the span that `resolve_in_memory` used to provide before `route`
// inlined it; the name is unchanged so existing traces still match.
#[instrument(
    name = "resolver::resolve_in_memory"
    skip(system_resolver, operations_payload, request_context)
)]
pub async fn resolve_in_memory_for_payload(
    operations_payload: OperationsPayload,
    system_resolver: &GraphQLSystemResolver,
    trusted_document_enforcement: TrustedDocumentEnforcement,
    request_context: &RequestContext<'_>,
) -> Result<Vec<(String, QueryResponse)>, SystemResolutionError> {
    let response = system_resolver
        .resolve_operations(
            operations_payload,
            request_context,
            trusted_document_enforcement,
        )
        .await;

    request_context
        .finalize_transaction(response.is_ok())
        .await
        .map_err(|e| {
            SystemResolutionError::Generic(format!("Error while finalizing transaction: {e}"))
        })
        .and(response)
}

#[cfg(test)]
mod expected_error_tests {
    use super::*;
    use async_graphql_parser::Pos;
    use core_resolver::validation::validation_error::ValidationError;

    fn pos() -> Pos {
        Pos { line: 1, column: 1 }
    }

    /// The families that made up almost all of the Sentry error volume: a stale
    /// or hand-written client selecting a field the schema does not have, and
    /// the access rules doing their job.
    #[test]
    fn client_mistakes_and_denials_are_expected() {
        let cases: Vec<SystemResolutionError> = vec![
            ValidationError::InvalidField("uuid".to_string(), "Tag".to_string(), pos()).into(),
            ValidationError::VariableNotFound("organizationUuid".to_string(), pos()).into(),
            SubsystemResolutionError::Authorization.into(),
            SubsystemResolutionError::InvalidField("locale".to_string(), "User").into(),
            SubsystemResolutionError::UserDisplayError("Invalid or expired code".to_string())
                .into(),
        ];

        for err in cases {
            assert!(
                is_expected_user_error(&err),
                "expected {err:?} to be treated as an ordinary outcome"
            );
        }
    }

    /// Anything that represents a fault the server did not intend must keep
    /// reaching Sentry — that is the signal the noise was burying.
    #[test]
    fn server_faults_are_still_captured() {
        let cases: Vec<SystemResolutionError> = vec![
            SystemResolutionError::NoResolverFound,
            SystemResolutionError::Generic("boom".to_string()),
            SystemResolutionError::AroundInterceptorReturnedNoResponse,
            SystemResolutionError::NoInterceptionTree,
            SubsystemResolutionError::NoInterceptorFound.into(),
        ];

        for err in cases {
            assert!(
                !is_expected_user_error(&err),
                "expected {err:?} to still be captured"
            );
        }
    }

    /// The escape hatch has to be off unless explicitly turned on, and must not
    /// be tripped by an unrelated value.
    #[test]
    fn capture_expected_errors_defaults_to_off() {
        // SAFETY: single-threaded test, restoring the variable before returning.
        unsafe {
            std::env::remove_var("EXO_SENTRY_CAPTURE_EXPECTED_ERRORS");
            assert!(!capture_expected_errors());

            std::env::set_var("EXO_SENTRY_CAPTURE_EXPECTED_ERRORS", "true");
            assert!(capture_expected_errors());

            std::env::set_var("EXO_SENTRY_CAPTURE_EXPECTED_ERRORS", "false");
            assert!(!capture_expected_errors());

            std::env::remove_var("EXO_SENTRY_CAPTURE_EXPECTED_ERRORS");
        }
    }
}
