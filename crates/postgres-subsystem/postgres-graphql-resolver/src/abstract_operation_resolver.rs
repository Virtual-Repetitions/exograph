// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use exo_sql::database_error::DatabaseError;

use common::context::RequestContext;
use core_resolver::{
    QueryResponse, QueryResponseBody, system_resolver::GraphQLSystemResolver,
    validation::field::ValidatedField,
};
use postgres_core_resolver::database_helper::extractor;

use postgres_core_resolver::postgres_execution_error::PostgresExecutionError;

use super::PostgresSubsystemResolver;
use crate::{
    computed_fields::apply_computed_fields_to_body, operation_resolver::PostgresResolvedOperation,
};

pub async fn resolve_operation<'e>(
    resolved_operation: PostgresResolvedOperation<'e>,
    field: &'e ValidatedField,
    subsystem_resolver: &'e PostgresSubsystemResolver,
    request_context: &'e RequestContext<'e>,
    system_resolver: &'e GraphQLSystemResolver,
) -> Result<QueryResponse, PostgresExecutionError> {
    let PostgresResolvedOperation {
        operation,
        return_type,
    } = resolved_operation;

    let mut tx = request_context
        .system_context
        .transaction_holder
        .try_lock()
        .unwrap();

    let result = subsystem_resolver
        .executor
        .execute(
            operation,
            &mut tx,
            &subsystem_resolver.subsystem.core_subsystem.database,
        )
        .await;

    if let Err(DatabaseError::Precheck(_)) = result {
        return Err(PostgresExecutionError::Authorization);
    }

    let mut result = result.map_err(PostgresExecutionError::Postgres)?;

    let body = if result.len() == 1 {
        let string_result = extractor(result.swap_remove(0))?;
        Ok(QueryResponseBody::Raw(Some(string_result)))
    } else if result.is_empty() {
        Ok(QueryResponseBody::Raw(None))
    } else {
        Err(PostgresExecutionError::NonUniqueResult(result.len()))
    }?;

    let mut response = QueryResponse {
        body,
        headers: vec![],
    };

    apply_computed_fields_to_body(
        &mut response.body,
        &return_type,
        field,
        subsystem_resolver,
        system_resolver,
        request_context,
    )
    .await?;

    Ok(response)
}
