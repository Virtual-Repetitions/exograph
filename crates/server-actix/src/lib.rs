// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

mod request;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use actix_web::{
    HttpRequest, HttpResponse, HttpResponseBuilder, Responder,
    web::{self, ServiceConfig},
};
use exo_env::Environment;
use reqwest::StatusCode;
use system_router::SystemRouter;
use url::Url;

use common::{
    env_const::{DeploymentMode, get_deployment_mode, get_graphql_http_path},
    router::Router,
};
use common::{
    http::{
        MemoryRequestHead, MemoryRequestPayload, RequestHead, RequestPayload, ResponseBody,
        ResponsePayload,
    },
    router::PlainRequestPayload,
};
use request::ActixRequestHead;
use serde_json::{Value, json};

const EXO_HEALTHZ_QUERY: &str = "EXO_HEALTHZ_QUERY";
const EXO_HEALTHZ_VARIABLES: &str = "EXO_HEALTHZ_VARIABLES";
const EXO_HEALTHZ_RESPONSE_JSON_POINTER: &str = "EXO_HEALTHZ_RESPONSE_JSON_POINTER";

macro_rules! error_msg {
    ($msg:literal) => {
        concat!("{\"errors\": [{\"message\":\"", $msg, "\"}]}").as_bytes()
    };
}

#[derive(Clone)]
struct GraphQLPaths {
    graphql_http_path: String,
}

pub fn configure_router(
    system_router: web::Data<SystemRouter>,
    env: Arc<dyn Environment>,
) -> impl FnOnce(&mut ServiceConfig) {
    let graphql_http_path = get_graphql_http_path(env.as_ref());
    let endpoint_url = match get_deployment_mode(env.as_ref()) {
        Ok(Some(DeploymentMode::Playground(url))) => {
            Some(Url::parse(&url).expect("Failed to parse upstream endpoint URL"))
        }
        _ => None,
    };

    move |app| {
        app.app_data(system_router)
            .app_data(web::Data::new(GraphQLPaths {
                graphql_http_path: graphql_http_path.clone(),
            }))
            .app_data(web::Data::new(env.clone()))
            .app_data(web::Data::new(endpoint_url))
            .default_service(web::to(resolve));
    }
}

/// Resolve a GraphQL request
///
/// # Arguments
/// * `endpoint_url` - The target URL for resolving data (None implies that the current server is also the target)
async fn resolve(
    http_request: HttpRequest,
    body: Option<web::Json<Value>>,
    query: web::Query<Value>,
    endpoint_url: web::Data<Option<Url>>,
    system_router: web::Data<SystemRouter>,
    env: web::Data<Arc<dyn Environment>>,
) -> impl Responder {
    if http_request.path() == "/healthz" && http_request.method() == actix_web::http::Method::GET {
        let graphql_http_path = http_request
            .app_data::<GraphQLPaths>()
            .map(|paths| paths.graphql_http_path.clone())
            .unwrap_or_else(|| "/graphql".to_string());

        return handle_healthz(
            system_router.get_ref(),
            env.as_ref().as_ref(),
            &graphql_http_path,
        )
        .await;
    }

    match endpoint_url.as_ref() {
        Some(endpoint_url) => {
            // In the playground mode, locally serve the schema query or playground assets
            let schema_query = http_request
                .headers()
                .get("_exo_operation_kind")
                .map(|v| v.as_bytes())
                == Some(b"schema_query");

            if schema_query
                || system_router.is_playground_assets_request(
                    http_request.path(),
                    to_reqwest_method(http_request.method()),
                )
            {
                resolve_locally(http_request, body, query.into_inner(), system_router).await
            } else {
                forward_request(http_request, body, endpoint_url).await
            }
        }
        None => {
            // We aren't operating in the playground mode, so we can resolve it locally
            resolve_locally(http_request, body, query.into_inner(), system_router).await
        }
    }
}

async fn handle_healthz(
    system_router: &SystemRouter,
    env: &dyn Environment,
    graphql_http_path: &str,
) -> HttpResponse {
    let default_query = "{ __typename }".to_string();
    let mut query = env.get(EXO_HEALTHZ_QUERY).unwrap_or_else(|| default_query.clone());
    let mut response_pointer = env.get(EXO_HEALTHZ_RESPONSE_JSON_POINTER);
    let variables = match env.get(EXO_HEALTHZ_VARIABLES) {
        Some(raw) => match expand_env_placeholders(&raw, env) {
            Ok(expanded) => match serde_json::from_str::<Value>(&expanded) {
                Ok(value) => Some(value),
                Err(err) => {
                    tracing::warn!(
                        "Invalid {} JSON; falling back to default health check: {}",
                        EXO_HEALTHZ_VARIABLES,
                        err
                    );
                    query = default_query;
                    response_pointer = None;
                    None
                }
            },
            Err(err) => {
                tracing::warn!(
                    "{}; falling back to default health check",
                    err
                );
                query = default_query;
                response_pointer = None;
                None
            }
        },
        None => {
            if env.get(EXO_HEALTHZ_QUERY).is_some() {
                tracing::warn!(
                    "{} not set; falling back to default health check",
                    EXO_HEALTHZ_VARIABLES
                );
                query = default_query;
                response_pointer = None;
            }
            None
        }
    };

    match execute_graphql_health_check(
        system_router,
        graphql_http_path,
        &query,
        variables,
        response_pointer.as_deref(),
    )
    .await
    {
        Ok(()) => HttpResponse::Ok().json(json!({ "status": "ok" })),
        Err(err) => {
            tracing::error!("GraphQL health check failed: {}", err);
            HttpResponse::ServiceUnavailable().json(json!({
                "status": "error",
                "message": err,
            }))
        }
    }
}

fn expand_env_placeholders(raw: &str, env: &dyn Environment) -> Result<String, String> {
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next();
            let mut name = String::new();
            for next in chars.by_ref() {
                if next == '}' {
                    break;
                }
                name.push(next);
            }

            if name.is_empty() {
                return Err("Invalid EXO_HEALTHZ_VARIABLES placeholder: empty name".to_string());
            }

            let value = env.get(&name).ok_or_else(|| {
                format!(
                    "Missing env var {} referenced by {}",
                    name, EXO_HEALTHZ_VARIABLES
                )
            })?;
            output.push_str(&value);
        } else {
            output.push(ch);
        }
    }

    Ok(output)
}

async fn execute_graphql_health_check(
    system_router: &SystemRouter,
    graphql_http_path: &str,
    query: &str,
    variables: Option<Value>,
    response_pointer: Option<&str>,
) -> Result<(), String> {
    let mut headers: HashMap<String, Vec<String>> = HashMap::new();
    headers.insert(
        "content-type".to_string(),
        vec!["application/json".to_string()],
    );
    headers.insert("_exo_playground".to_string(), vec!["true".to_string()]);

    let request_head = MemoryRequestHead::new(
        headers,
        HashMap::new(),
        http::Method::POST,
        graphql_http_path.to_string(),
        Value::Null,
        None,
    );

    let body = match variables {
        Some(variables) => json!({
            "query": query,
            "variables": variables,
        }),
        None => json!({
            "query": query,
        }),
    };

    let payload = MemoryRequestPayload::new(body, request_head);
    let request_payload = PlainRequestPayload::external(Box::new(payload));

    match system_router.route(&request_payload).await {
        Some(response) if response.status_code.is_success() => {
            let response_json = response
                .body
                .to_json()
                .await
                .map_err(|err| format!("Invalid GraphQL response body: {}", err))?;

            if response_json.get("errors").is_some() {
                return Err("GraphQL response contains errors".to_string());
            }

            if let Some(pointer) = response_pointer {
                match response_json.pointer(pointer) {
                    Some(Value::Bool(true)) => Ok(()),
                    Some(other) => Err(format!(
                        "Health check JSON pointer {} did not evaluate to true (got {})",
                        pointer, other
                    )),
                    None => Err(format!(
                        "Health check JSON pointer {} not found in response",
                        pointer
                    )),
                }
            } else {
                Ok(())
            }
        }
        Some(response) => Err(format!(
            "Unexpected status {} from GraphQL endpoint",
            response.status_code
        )),
        None => Err("GraphQL endpoint returned no response".to_string()),
    }
}

struct ActixRequestPayload {
    head: ActixRequestHead,
    body: Mutex<Value>,
}

impl RequestPayload for ActixRequestPayload {
    fn get_head(&self) -> &(dyn RequestHead + Send + Sync) {
        &self.head
    }

    fn take_body(&self) -> Value {
        self.body.lock().unwrap().take()
    }
}

async fn resolve_locally(
    req: HttpRequest,
    body: Option<web::Json<Value>>,
    query: Value,
    system_router: web::Data<SystemRouter>,
) -> HttpResponse {
    let request = ActixRequestPayload {
        head: ActixRequestHead::from_request(req, query),
        body: Mutex::new(body.map(|b| b.into_inner()).unwrap_or(Value::Null)),
    };

    let response = system_router
        .route(&PlainRequestPayload::external(Box::new(request)))
        .await;

    match response {
        Some(ResponsePayload {
            body,
            headers,
            status_code,
        }) => {
            let actix_status_code = match to_actix_status_code(status_code) {
                Ok(status_code) => status_code,
                Err(err) => {
                    tracing::error!("Invalid status code: {}", err);
                    return HttpResponse::build(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR)
                        .body(error_msg!("Invalid status code"));
                }
            };

            let mut builder = HttpResponse::build(actix_status_code);

            for header in headers.into_iter() {
                builder.append_header(header);
            }

            match body {
                ResponseBody::Stream(stream) => builder.streaming(stream),
                ResponseBody::Bytes(bytes) => builder.body(bytes),
                ResponseBody::Redirect(url) => builder.append_header(("Location", url)).body(""),
                ResponseBody::None => builder.body(""),
            }
        }
        None => HttpResponse::build(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR)
            .body(error_msg!("Error resolving request")),
    }
}

async fn forward_request(
    req: HttpRequest,
    body: Option<web::Json<Value>>,
    forward_url: &Url,
) -> HttpResponse {
    let mut forward_url = forward_url.clone();
    forward_url.set_query(req.uri().query());

    let body = body
        .map(|b| b.into_inner().to_string())
        .unwrap_or("".to_string());

    let forwarded_req = reqwest::Client::default()
        .request(to_reqwest_method(req.method()), forward_url)
        .body(body);

    let forwarded_req = req
        .headers()
        .iter()
        .filter(|(h, _)| *h != "origin" && *h != "connection" && *h != "host")
        .fold(forwarded_req, |forwarded_req, (h, v)| {
            forwarded_req.header(h.as_str(), v.as_bytes())
        });

    let res = match forwarded_req.send().await {
        Ok(res) => res,
        Err(err) => {
            tracing::error!("Error forwarding request to the endpoint: {}", err);
            return HttpResponse::InternalServerError()
                .body(error_msg!("Error forwarding request to the endpoint"));
        }
    };

    let mut client_resp = HttpResponseBuilder::new(to_actix_status_code(res.status()).unwrap());

    for (header_name, header_value) in res.headers().iter().filter(|(h, _)| *h != "connection") {
        client_resp.insert_header((header_name.as_str(), header_value.as_bytes()));
    }

    match res.bytes().await {
        Ok(bytes) => client_resp.body(bytes),
        Err(err) => {
            tracing::error!("Error reading response body from endpoint: {}", err);
            client_resp.body(error_msg!("Error reading response body from endpoint"))
        }
    }
}

fn to_actix_status_code(status_code: StatusCode) -> Result<actix_web::http::StatusCode, String> {
    actix_web::http::StatusCode::from_u16(status_code.as_u16())
        .map_err(|_| "Invalid status code".to_string())
}

// Actix uses http-0.2. However, the rest of the system uses
// http-1.x, so we need to convert between the two.
// Once Actix 5.x is released (which uses http-1.x), we can remove this mapping.
fn to_reqwest_method(method: &actix_web::http::Method) -> reqwest::Method {
    match *method {
        actix_web::http::Method::CONNECT => reqwest::Method::CONNECT,
        actix_web::http::Method::GET => reqwest::Method::GET,
        actix_web::http::Method::HEAD => reqwest::Method::HEAD,
        actix_web::http::Method::OPTIONS => reqwest::Method::OPTIONS,
        actix_web::http::Method::POST => reqwest::Method::POST,
        actix_web::http::Method::PUT => reqwest::Method::PUT,
        actix_web::http::Method::DELETE => reqwest::Method::DELETE,
        actix_web::http::Method::PATCH => reqwest::Method::PATCH,
        actix_web::http::Method::TRACE => reqwest::Method::TRACE,
        _ => {
            tracing::error!("Unsupported method: {}", method);
            panic!("Unsupported method: {}", method);
        }
    }
}
