// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! When the introspection gate is configured (the playground's GitHub-OAuth
//! gate and/or `EXO_INTROSPECTION_SECRET`), introspection queries must be held
//! to it: a hosted playground needs introspection enabled, and gating only the
//! playground HTML would leave the schema publicly fetchable via introspection
//! queries against the GraphQL endpoint. Headless clients (codegen, schema
//! diffing) present the shared secret instead of a browser session — see
//! `common::introspection_auth` for the accept rules.
//!
//! Root-level `__typename` is exempt: it resolves through this subsystem too,
//! but returns the literal, schema-independent string `"Query"` (or
//! `"Mutation"`), discloses nothing the gate protects, and is the conventional
//! GraphQL liveness ping — gating it turns monitors' `{ __typename }` probes
//! into `Not authorized` failures that read as outages.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use common::context::RequestContext;
use common::http::RequestPayload;
use common::introspection_auth::IntrospectionAuthConfig;
use core_plugin_shared::interception::InterceptorIndex;
use core_resolver::plugin::{SubsystemGraphQLResolver, SubsystemResolutionError};
use core_resolver::system_resolver::GraphQLSystemResolver;
use core_resolver::{InterceptedOperation, QueryResponse, validation::field::ValidatedField};

use async_graphql_parser::types::{FieldDefinition, OperationType, TypeDefinition};

pub struct IntrospectionGuardedResolver {
    inner: Arc<dyn SubsystemGraphQLResolver + Send + Sync>,
    auth_config: IntrospectionAuthConfig,
}

impl IntrospectionGuardedResolver {
    pub fn new(
        inner: Arc<dyn SubsystemGraphQLResolver + Send + Sync>,
        auth_config: IntrospectionAuthConfig,
    ) -> Self {
        Self { inner, auth_config }
    }
}

#[async_trait]
impl SubsystemGraphQLResolver for IntrospectionGuardedResolver {
    fn id(&self) -> &'static str {
        "introspection-auth-guarded"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn resolve<'a>(
        &'a self,
        operation: &'a ValidatedField,
        operation_type: OperationType,
        request_context: &'a RequestContext,
        system_resolver: &'a GraphQLSystemResolver,
    ) -> Result<Option<QueryResponse>, SubsystemResolutionError> {
        if operation.name.as_str() != "__typename"
            && !self.auth_config.authorize(request_context.get_head())
        {
            return Err(SubsystemResolutionError::Authorization);
        }
        self.inner
            .resolve(operation, operation_type, request_context, system_resolver)
            .await
    }

    async fn invoke_interceptor<'a>(
        &'a self,
        interceptor_index: InterceptorIndex,
        intercepted_operation: &'a InterceptedOperation,
        request_context: &'a RequestContext<'a>,
        system_resolver: &'a GraphQLSystemResolver,
    ) -> Result<Option<QueryResponse>, SubsystemResolutionError> {
        self.inner
            .invoke_interceptor(
                interceptor_index,
                intercepted_operation,
                request_context,
                system_resolver,
            )
            .await
    }

    fn schema_queries(&self) -> Vec<FieldDefinition> {
        self.inner.schema_queries()
    }

    fn schema_mutations(&self) -> Vec<FieldDefinition> {
        self.inner.schema_mutations()
    }

    fn schema_types(&self) -> Vec<TypeDefinition> {
        self.inner.schema_types()
    }
}
