// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use super::predicate_mapper::compute_predicate;
use super::{
    auth_util::{AccessCheckOutcome, check_access, check_retrieve_access},
    sql_mapper::SQLOperationKind,
    util::Arguments,
};
use crate::util::to_pg_vector;
use crate::{
    operation_resolver::{OperationSelectionResolver, ResolvedSelect},
    order_by_mapper::{OrderByComputation, OrderByParameterInput},
    sql_mapper::extract_and_map,
};
use async_graphql_value::Name;
use async_recursion::async_recursion;
use async_trait::async_trait;
use common::context::RequestContext;
use core_model::types::OperationReturnType;
use core_resolver::validation::field::ValidatedField;
use exo_sql::{
    AbstractOrderBy, AbstractPredicate, AbstractSelect, AliasedSelectionElement, Limit, Offset,
    RelationId, Selection, SelectionCardinality, SelectionElement,
};
use exo_sql::{Function, SQLParamContainer};
use futures::StreamExt;
use indexmap::IndexMap;
use postgres_core_model::vector_distance::VectorDistanceField;
use postgres_core_model::{
    aggregate::AggregateField,
    relation::{
        ManyToOneRelation, OneToManyRelation, PostgresRelation, RelationCardinality,
        TransitiveRelation, TransitiveRelationStep,
    },
    types::{EntityType, PostgresField},
};
use postgres_core_resolver::postgres_execution_error::PostgresExecutionError;
use postgres_graphql_model::query::UniqueQuery;
use postgres_graphql_model::{
    order::OrderByParameter,
    query::{CollectionQuery, CollectionQueryParameters},
    subsystem::PostgresGraphQLSubsystem,
};
use std::collections::HashSet;

#[async_trait]
impl OperationSelectionResolver for UniqueQuery {
    async fn resolve_select<'a>(
        &'a self,
        field: &'a ValidatedField,
        request_context: &'a RequestContext<'a>,
        subsystem: &'a PostgresGraphQLSubsystem,
    ) -> Result<ResolvedSelect<'a>, PostgresExecutionError> {
        let return_entity_type = self.return_type.typ(&subsystem.core_subsystem.entity_types);
        let parent_read_predicate = check_retrieve_access(
            &subsystem.core_subsystem.database_access_expressions[return_entity_type.access.read],
            subsystem,
            request_context,
        )
        .await?;
        let restrict_relations = parent_read_predicate != AbstractPredicate::True;

        let predicate = compute_predicate(
            &self.parameters.predicate_params.iter().collect::<Vec<_>>(),
            &field.arguments,
            subsystem,
            request_context,
            restrict_relations,
        )
        .await?;

        let select = compute_select(
            predicate,
            None,
            None,
            None,
            &self.return_type,
            &field.subfields,
            subsystem,
            request_context,
        )
        .await?;

        Ok(ResolvedSelect {
            select,
            return_type: &self.return_type,
        })
    }
}

#[async_trait]
impl OperationSelectionResolver for CollectionQuery {
    async fn resolve_select<'a>(
        &'a self,
        field: &'a ValidatedField,
        request_context: &'a RequestContext<'a>,
        subsystem: &'a PostgresGraphQLSubsystem,
    ) -> Result<ResolvedSelect<'a>, PostgresExecutionError> {
        let CollectionQueryParameters {
            predicate_param,
            order_by_param,
            limit_param,
            offset_param,
        } = &self.parameters;

        let arguments = &field.arguments;

        let return_entity_type = self.return_type.typ(&subsystem.core_subsystem.entity_types);
        let parent_read_predicate = check_retrieve_access(
            &subsystem.core_subsystem.database_access_expressions[return_entity_type.access.read],
            subsystem,
            request_context,
        )
        .await?;
        let restrict_relations = parent_read_predicate != AbstractPredicate::True;

        let base_predicate = compute_predicate(
            &[predicate_param],
            arguments,
            subsystem,
            request_context,
            restrict_relations,
        )
        .await?;

        let order_by_result =
            compute_order_by(order_by_param, arguments, subsystem, request_context).await?;

        let (order_by, order_by_predicate) = match order_by_result {
            Some(OrderByComputation {
                order_by,
                predicate,
            }) => (Some(order_by), predicate),
            None => (None, AbstractPredicate::True),
        };

        let combined_predicate = AbstractPredicate::and(base_predicate, order_by_predicate);

        let select = compute_select(
            combined_predicate,
            order_by,
            extract_and_map(limit_param, arguments, subsystem, request_context).await?,
            extract_and_map(offset_param, arguments, subsystem, request_context).await?,
            &self.return_type,
            &field.subfields,
            subsystem,
            request_context,
        )
        .await?;

        Ok(ResolvedSelect {
            select,
            return_type: &self.return_type,
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn compute_select<'content>(
    predicate: AbstractPredicate,
    order_by: Option<AbstractOrderBy>,
    limit: Option<Limit>,
    offset: Option<Offset>,
    return_type: &'content OperationReturnType<EntityType>,
    selection: &'content [ValidatedField],
    subsystem: &'content PostgresGraphQLSubsystem,
    request_context: &'content RequestContext<'content>,
) -> Result<AbstractSelect, PostgresExecutionError> {
    let return_entity_type = return_type.typ(&subsystem.core_subsystem.entity_types);

    let AccessCheckOutcome {
        precheck_predicate: _,
        entity_predicate,
        unauthorized_fields,
    } = check_access(
        return_entity_type,
        selection,
        &SQLOperationKind::Retrieve,
        subsystem,
        request_context,
        None,
    )
    .await?;

    let predicate = AbstractPredicate::and(predicate, entity_predicate);

    let unauthorized_set = unauthorized_fields.iter().cloned().collect::<HashSet<_>>();

    let content_object = content_select(
        return_entity_type,
        selection,
        &unauthorized_set,
        subsystem,
        request_context,
    )
    .await?;

    let selection_cardinality = match return_type {
        OperationReturnType::List(_) => SelectionCardinality::Many,
        _ => SelectionCardinality::One,
    };
    Ok(AbstractSelect {
        table_id: return_entity_type.table_id,
        selection: exo_sql::Selection::Json(content_object, selection_cardinality),
        predicate,
        order_by,
        offset,
        limit,
    })
}

async fn compute_order_by<'content>(
    param: &'content OrderByParameter,
    arguments: &'content Arguments,
    subsystem: &'content PostgresGraphQLSubsystem,
    request_context: &'content RequestContext<'content>,
) -> Result<Option<OrderByComputation>, PostgresExecutionError> {
    extract_and_map(
        OrderByParameterInput {
            param,
            parent_column_path: None,
        },
        arguments,
        subsystem,
        request_context,
    )
    .await
}

#[async_recursion]
async fn content_select<'content>(
    return_type: &EntityType,
    fields: &'content [ValidatedField],
    unauthorized_fields: &HashSet<String>,
    subsystem: &'content PostgresGraphQLSubsystem,
    request_context: &'content RequestContext<'content>,
) -> Result<Vec<AliasedSelectionElement>, PostgresExecutionError> {
    futures::stream::iter(fields.iter())
        .then(|field| async {
            map_field(
                return_type,
                field,
                unauthorized_fields,
                subsystem,
                request_context,
            )
            .await
        })
        .collect::<Vec<Result<_, _>>>()
        .await
        .into_iter()
        .collect()
}

async fn map_field<'content>(
    return_type: &EntityType,
    field: &'content ValidatedField,
    unauthorized_fields: &HashSet<String>,
    subsystem: &'content PostgresGraphQLSubsystem,
    request_context: &'content RequestContext<'content>,
) -> Result<AliasedSelectionElement, PostgresExecutionError> {
    let output_name = field.output_name();

    let selection_elem = if unauthorized_fields.contains(output_name.as_str()) {
        SelectionElement::Null
    } else if field.name == "__typename" {
        SelectionElement::Constant(return_type.name.to_owned())
    } else {
        let entity_field = return_type.field_by_name(&field.name);

        match entity_field {
            Some(entity_field) => {
                map_persistent_field(entity_field, field, subsystem, request_context).await?
            }
            None => {
                let agg_field = return_type.aggregate_field_by_name(&field.name);
                match agg_field {
                    Some(agg_field) => {
                        map_aggregate_field(agg_field, field, subsystem, request_context).await?
                    }
                    None => {
                        let vector_distance_field = return_type
                            .vector_distance_field_by_name(&field.name)
                            .ok_or_else(|| {
                                PostgresExecutionError::Generic(format!(
                                    "Unknown field '{}' on type '{}'",
                                    field.name, return_type.name
                                ))
                            })?;

                        map_vector_distance_field(vector_distance_field, field).await?
                    }
                }
            }
        }
    };

    Ok(AliasedSelectionElement::new(output_name, selection_elem))
}

async fn map_persistent_field<'content>(
    entity_field: &PostgresField<EntityType>,
    field: &'content ValidatedField,
    subsystem: &'content PostgresGraphQLSubsystem,
    request_context: &'content RequestContext<'content>,
) -> Result<SelectionElement, PostgresExecutionError> {
    match &entity_field.relation {
        PostgresRelation::Scalar { column_id, .. } => Ok(SelectionElement::Physical(*column_id)),
        PostgresRelation::ManyToOne { relation, .. } => {
            let ManyToOneRelation {
                foreign_entity_id, ..
            } = relation;

            let foreign_table_pk_query = subsystem.get_pk_query(*foreign_entity_id);

            let nested_abstract_select = foreign_table_pk_query
                .resolve_select(field, request_context, subsystem)
                .await?;

            Ok(SelectionElement::SubSelect(
                RelationId::ManyToOne(relation.relation_id),
                Box::new(nested_abstract_select.select),
            ))
        }
        PostgresRelation::OneToMany(relation) => {
            let OneToManyRelation {
                foreign_entity_id,
                cardinality,
                ..
            } = relation;

            let nested_abstract_select = {
                // Get an appropriate query based on the cardinality of the relation
                if cardinality == &RelationCardinality::Unbounded {
                    let collection_query = subsystem.get_collection_query(*foreign_entity_id);

                    collection_query
                        .resolve_select(field, request_context, subsystem)
                        .await?
                } else {
                    let pk_query = subsystem.get_pk_query(*foreign_entity_id);

                    pk_query
                        .resolve_select(field, request_context, subsystem)
                        .await?
                }
            };

            Ok(SelectionElement::SubSelect(
                RelationId::OneToMany(relation.relation_id),
                Box::new(nested_abstract_select.select),
            ))
        }
        PostgresRelation::Computed(computed) => {
            if computed.dependencies.is_empty() {
                Ok(SelectionElement::Null)
            } else {
                let object_fields = computed
                    .dependencies
                    .iter()
                    .map(|dependency| {
                        (
                            dependency.field_name.clone(),
                            SelectionElement::Physical(dependency.column_id),
                        )
                    })
                    .collect();
                Ok(SelectionElement::Object(object_fields))
            }
        }
        PostgresRelation::Embedded => {
            panic!("Embedded relations cannot be used in queries")
        }
        PostgresRelation::Transitive(transitive) => {
            map_transitive_field(transitive, field, subsystem, request_context).await
        }
    }
}

const TRANSITIVE_VALUE_ALIAS: &str = "__transitive_value";

async fn map_transitive_field<'content>(
    transitive: &'content TransitiveRelation,
    field: &'content ValidatedField,
    subsystem: &'content PostgresGraphQLSubsystem,
    request_context: &'content RequestContext<'content>,
) -> Result<SelectionElement, PostgresExecutionError> {
    if transitive.steps.is_empty() {
        return Err(PostgresExecutionError::Generic(
            "Validation error: Transitive relation must contain at least one step".to_string(),
        ));
    }

    let step_fields = build_transitive_step_fields(transitive, field);

    let mut current_selection: Option<SelectionElement> = None;

    for (idx, step) in (0..transitive.steps.len())
        .rev()
        .map(|i| (i, &transitive.steps[i]))
    {
        let step_field = &step_fields[idx];
        let mut resolved_select =
            resolve_select_for_transitive_step(step, step_field, subsystem, request_context)
                .await?;

        if let Some(inner_selection) = current_selection.take() {
            let selection_cardinality = selection_cardinality_for_step(step);
            resolved_select.select.selection = Selection::Json(
                vec![AliasedSelectionElement::new(
                    TRANSITIVE_VALUE_ALIAS.to_string(),
                    inner_selection,
                )],
                selection_cardinality,
            );

            let sub_select =
                SelectionElement::SubSelect(step.relation_id, Box::new(resolved_select.select));

            let flattened = match (&step.relation_id, &step.cardinality) {
                (RelationId::OneToMany(_), RelationCardinality::Unbounded) => {
                    SelectionElement::JsonArrayExtract {
                        source: Box::new(sub_select),
                        key: TRANSITIVE_VALUE_ALIAS.to_string(),
                    }
                }
                _ => SelectionElement::JsonExtract {
                    source: Box::new(sub_select),
                    path: vec![TRANSITIVE_VALUE_ALIAS.to_string()],
                },
            };

            current_selection = Some(flattened);
        } else {
            current_selection = Some(SelectionElement::SubSelect(
                step.relation_id,
                Box::new(resolved_select.select),
            ));
        }
    }

    current_selection.ok_or_else(|| {
        PostgresExecutionError::Generic(
            "Validation error: Failed to resolve transitive relation".to_string(),
        )
    })
}

fn build_transitive_step_fields(
    transitive: &TransitiveRelation,
    leaf_field: &ValidatedField,
) -> Vec<ValidatedField> {
    fn clone_field(field: &ValidatedField) -> ValidatedField {
        ValidatedField {
            alias: field.alias.clone(),
            name: field.name.clone(),
            arguments: field.arguments.clone(),
            subfields: field.subfields.iter().map(clone_field).collect(),
        }
    }

    let mut step_fields: Vec<ValidatedField> = Vec::with_capacity(transitive.steps.len());

    if transitive.steps.is_empty() {
        return step_fields;
    }

    let last_step = transitive.steps.last().expect("At least one step");

    let mut current = ValidatedField {
        alias: leaf_field.alias.clone(),
        name: Name::new(last_step.field_name.clone()),
        arguments: leaf_field.arguments.clone(),
        subfields: leaf_field.subfields.iter().map(clone_field).collect(),
    };

    step_fields.push(clone_field(&current));

    for step in transitive.steps.iter().rev().skip(1) {
        let parent = ValidatedField {
            alias: None,
            name: Name::new(step.field_name.clone()),
            arguments: IndexMap::new(),
            subfields: vec![current],
        };
        current = parent;
        step_fields.push(clone_field(&current));
    }

    step_fields.reverse();
    step_fields
}

fn selection_cardinality_for_step(step: &TransitiveRelationStep) -> SelectionCardinality {
    match step.relation_id {
        RelationId::ManyToOne(_) => SelectionCardinality::One,
        RelationId::OneToMany(_) => match step.cardinality {
            RelationCardinality::Unbounded => SelectionCardinality::Many,
            RelationCardinality::Optional => SelectionCardinality::One,
        },
    }
}

async fn resolve_select_for_transitive_step<'content>(
    step: &TransitiveRelationStep,
    step_field: &'content ValidatedField,
    subsystem: &'content PostgresGraphQLSubsystem,
    request_context: &'content RequestContext<'content>,
) -> Result<ResolvedSelect<'content>, PostgresExecutionError> {
    match step.relation_id {
        RelationId::ManyToOne(_) => {
            subsystem
                .get_pk_query(step.entity_id)
                .resolve_select(step_field, request_context, subsystem)
                .await
        }
        RelationId::OneToMany(_) => {
            if matches!(step.cardinality, RelationCardinality::Unbounded) {
                subsystem
                    .get_collection_query(step.entity_id)
                    .resolve_select(step_field, request_context, subsystem)
                    .await
            } else {
                subsystem
                    .get_pk_query(step.entity_id)
                    .resolve_select(step_field, request_context, subsystem)
                    .await
            }
        }
    }
}

async fn map_aggregate_field<'content>(
    agg_field: &AggregateField,
    field: &'content ValidatedField,
    subsystem: &'content PostgresGraphQLSubsystem,
    request_context: &'content RequestContext<'content>,
) -> Result<SelectionElement, PostgresExecutionError> {
    if let Some(PostgresRelation::OneToMany(relation)) = &agg_field.relation {
        let OneToManyRelation {
            foreign_entity_id,
            cardinality,
            relation_id,
        } = relation;
        // TODO: Avoid code duplication with map_persistent_field

        let nested_abstract_select = {
            // Aggregate is supported only for unbounded relations (i.e. not supported for one-to-one)
            if cardinality == &RelationCardinality::Unbounded {
                let aggregate_query = subsystem.get_aggregate_query(*foreign_entity_id);

                aggregate_query
                    .resolve_select(field, request_context, subsystem)
                    .await
            } else {
                // Reaching this point means our validation logic failed
                Err(PostgresExecutionError::Generic(
                    "Validation error: Aggregate is supported only for unbounded relations"
                        .to_string(),
                ))
            }
        }?;

        Ok(SelectionElement::SubSelect(
            RelationId::OneToMany(*relation_id),
            Box::new(nested_abstract_select.select),
        ))
    } else {
        // Reaching this point means our validation logic failed
        Err(PostgresExecutionError::Generic(
            "Validation error: Aggregate is supported only for one-to-many".to_string(),
        ))
    }
}

async fn map_vector_distance_field(
    vector_distance_field: &VectorDistanceField,
    field: &ValidatedField,
) -> Result<SelectionElement, PostgresExecutionError> {
    let to_arg = field.arguments.get("to").ok_or_else(|| {
        PostgresExecutionError::Generic(
            "Missing 'to' argument for vector distance field".to_string(),
        )
    })?;

    let to_vector_value = to_pg_vector(to_arg, "to")?;

    Ok(SelectionElement::Function(Function::VectorDistance {
        column_id: vector_distance_field.column_id,
        distance_function: vector_distance_field.distance_function,
        target: SQLParamContainer::f32_array(to_vector_value),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::create_postgres_system_from_str;
    use async_graphql_value::Name;
    use async_trait::async_trait;
    use common::{
        context::RequestContext,
        http::{RequestHead, RequestPayload, ResponsePayload},
        router::{PlainRequestPayload, Router},
    };
    use exo_env::MapEnvironment;
    use http::Method;
    use indexmap::IndexMap;

    struct DummyRequest;

    impl RequestPayload for DummyRequest {
        fn get_head(&self) -> &(dyn RequestHead + Send + Sync) {
            self
        }

        fn take_body(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
    }

    impl RequestHead for DummyRequest {
        fn get_headers(&self, _key: &str) -> Vec<String> {
            vec![]
        }

        fn get_ip(&self) -> Option<std::net::IpAddr> {
            None
        }

        fn get_method(&self) -> Method {
            Method::POST
        }

        fn get_path(&self) -> String {
            String::new()
        }

        fn get_query(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
    }

    struct DummyRouter;

    #[async_trait]
    impl<'request> Router<PlainRequestPayload<'request>> for DummyRouter {
        async fn route(
            &self,
            _request_context: &PlainRequestPayload<'request>,
        ) -> Option<ResponsePayload> {
            None
        }
    }

    fn validated_field(name: &str, subfields: Vec<ValidatedField>) -> ValidatedField {
        ValidatedField {
            alias: None,
            name: Name::new(name),
            arguments: IndexMap::new(),
            subfields,
        }
    }

    #[tokio::test]
    async fn transitive_set_field_uses_array_extraction() {
        let subsystem = create_postgres_system_from_str(
            r#"
            @postgres
            module Library {
                @access(true)
                type Author {
                    @pk id: Int = autoIncrement()
                    name: String
                    @relation("author")
                    books: Set<BookAuthor>
                    @manyToOne
                    favoritePublisher: Publisher?
                    @relationPath("books.publisher")
                    publishers: Set<Publisher>
                }

                @access(true)
                type BookAuthor {
                    @pk id: Int = autoIncrement()
                    author: Author
                    @manyToOne
                    publisher: Publisher
                    @relationPath("author.favoritePublisher")
                    authorFavoritePublisher: Publisher?
                }

                @access(true)
                type Publisher {
                    @pk id: Int = autoIncrement()
                    name: String
                }
            }
            "#,
            "library.exo".to_string(),
        )
        .await
        .expect("Failed to build subsystem");

        let env = MapEnvironment::new();
        let router = DummyRouter;
        const REQUEST: DummyRequest = DummyRequest;
        let jwt: Option<common::context::JwtAuthenticator> = None;
        let request_context = RequestContext::new(&REQUEST, vec![], &router, &jwt, &env);

        let (_, author_entity) = subsystem
            .core_subsystem
            .entity_types
            .iter()
            .find(|(_, entity)| entity.name == "Author")
            .expect("Author entity not found");

        let publishers_field = author_entity
            .field_by_name("publishers")
            .expect("publishers field missing");

        match &publishers_field.relation {
            PostgresRelation::Transitive(transitive) => {
                assert_eq!(transitive.steps.len(), 2);
            }
            other => panic!("Expected transitive relation, found {:?}", other),
        }

        let publishers_validated_field =
            validated_field("publishers", vec![validated_field("name", vec![])]);

        let selection = map_persistent_field(
            publishers_field,
            &publishers_validated_field,
            &subsystem,
            &request_context,
        )
        .await
        .expect("Mapping transitive field failed");

        match selection {
            SelectionElement::JsonArrayExtract { key, source } => {
                assert_eq!(key, TRANSITIVE_VALUE_ALIAS);
                assert!(matches!(*source, SelectionElement::SubSelect(_, _)));
            }
            other => panic!("Expected JsonArrayExtract, found {:?}", other),
        }
    }

    #[tokio::test]
    async fn transitive_single_field_uses_value_extraction() {
        let subsystem = create_postgres_system_from_str(
            r#"
            @postgres
            module Library {
                @access(true)
                type Author {
                    @pk id: Int = autoIncrement()
                    name: String
                    @relation("author")
                    books: Set<BookAuthor>
                    @manyToOne
                    favoritePublisher: Publisher?
                }

                @access(true)
                type BookAuthor {
                    @pk id: Int = autoIncrement()
                    author: Author
                    @manyToOne
                    publisher: Publisher
                    @relationPath("author.favoritePublisher")
                    authorFavoritePublisher: Publisher?
                }

                @access(true)
                type Publisher {
                    @pk id: Int = autoIncrement()
                    name: String
                }
            }
            "#,
            "library-single.exo".to_string(),
        )
        .await
        .expect("Failed to build subsystem");

        let env = MapEnvironment::new();
        let router = DummyRouter;
        const REQUEST: DummyRequest = DummyRequest;
        let jwt: Option<common::context::JwtAuthenticator> = None;
        let request_context = RequestContext::new(&REQUEST, vec![], &router, &jwt, &env);

        let (_, book_author_entity) = subsystem
            .core_subsystem
            .entity_types
            .iter()
            .find(|(_, entity)| entity.name == "BookAuthor")
            .expect("BookAuthor entity not found");

        let favorite_field = book_author_entity
            .field_by_name("authorFavoritePublisher")
            .expect("authorFavoritePublisher field missing");

        let favorite_validated_field = validated_field(
            "authorFavoritePublisher",
            vec![validated_field("name", vec![])],
        );

        let selection = map_persistent_field(
            favorite_field,
            &favorite_validated_field,
            &subsystem,
            &request_context,
        )
        .await
        .expect("Mapping transitive field failed");

        match selection {
            SelectionElement::JsonExtract { path, .. } => {
                assert_eq!(path, vec![TRANSITIVE_VALUE_ALIAS.to_string()]);
            }
            other => panic!("Expected JsonExtract, found {:?}", other),
        }
    }

    #[tokio::test]
    async fn relation_path_requires_collection_type_for_unbounded_path() {
        let result = builder::build_system_from_str(
            r#"
            @postgres
            module InvalidCollection {
                @access(true)
                type Author {
                    @pk id: Int = autoIncrement()
                    name: String
                    @relation("author")
                    books: Set<Book>
                    @relationPath("books.publisher")
                    publishers: Publisher?
                }

                @access(true)
                type Book {
                    @pk id: Int = autoIncrement()
                    author: Author
                    @manyToOne
                    publisher: Publisher
                }

                @access(true)
                type Publisher {
                    @pk id: Int = autoIncrement()
                    name: String
                }
            }
            "#,
            "invalid-collection.exo".to_string(),
            vec![Box::new(
                postgres_builder::PostgresSubsystemBuilder::default(),
            )],
            core_model_builder::plugin::BuildMode::Build,
        )
        .await;

        let err = result.expect_err("Schema with invalid collection type should fail");
        let error_message = format!("{err}");
        assert!(
            error_message.contains(
                "resolves @relationPath to a collection but is declared as a single value"
            ),
            "Unexpected error message: {error_message}"
        );
    }

    #[tokio::test]
    async fn relation_path_rejects_collection_for_single_path() {
        let result = builder::build_system_from_str(
            r#"
            @postgres
            module InvalidSingle {
                @access(true)
                type Author {
                    @pk id: Int = autoIncrement()
                    name: String
                    @manyToOne
                    favoritePublisher: Publisher?
                    @relationPath("favoritePublisher")
                    favoritePublishers: Set<Publisher>
                }

                @access(true)
                type Publisher {
                    @pk id: Int = autoIncrement()
                    name: String
                }
            }
            "#,
            "invalid-single.exo".to_string(),
            vec![Box::new(
                postgres_builder::PostgresSubsystemBuilder::default(),
            )],
            core_model_builder::plugin::BuildMode::Build,
        )
        .await;

        let err = result.expect_err("Schema with mismatched single relation should fail");
        let error_message = format!("{err}");
        assert!(
            error_message.contains(
                "resolves @relationPath to a single value but is declared as a collection"
            ),
            "Unexpected error message: {error_message}"
        );
    }

    #[tokio::test]
    async fn relation_path_requires_optional_for_nullable_path() {
        let result = builder::build_system_from_str(
            r#"
            @postgres
            module InvalidOptional {
                @access(true)
                type Library {
                    @pk id: Int = autoIncrement()
                    featuredAuthor: Author?
                    @relationPath("featuredAuthor.books")
                    featuredBooks: Set<Book>
                }

                @access(true)
                type Author {
                    @pk id: Int = autoIncrement()
                    name: String
                    @relation("featuredAuthor")
                    featuredIn: Set<Library>?
                    @relation("author")
                    books: Set<Book>
                }

                @access(true)
                type Book {
                    @pk id: Int = autoIncrement()
                    title: String
                    author: Author
                }
            }
            "#,
            "invalid-optional.exo".to_string(),
            vec![Box::new(
                postgres_builder::PostgresSubsystemBuilder::default(),
            )],
            core_model_builder::plugin::BuildMode::Build,
        )
        .await;

        let err = result.expect_err("Schema requiring optional relation should fail");
        let error_message = format!("{err}");
        assert!(
            error_message.contains(
                "resolves @relationPath to an optional value but is declared as non-optional"
            ),
            "Unexpected error message: {error_message}"
        );
    }

    #[tokio::test]
    async fn transitive_optional_path_cardinality_identified() {
        let subsystem = create_postgres_system_from_str(
            r#"
            @postgres
            module OptionalPath {
                @access(true)
                type Library {
                    @pk id: Int = autoIncrement()
                    featuredAuthor: Author?
                    @relationPath("featuredAuthor.books")
                    featuredBooks: Set<Book>?
                }

                @access(true)
                type Author {
                    @pk id: Int = autoIncrement()
                    name: String
                    @relation("featuredAuthor")
                    featuredIn: Set<Library>?
                    @relation("author")
                    books: Set<Book>
                }

                @access(true)
                type Book {
                    @pk id: Int = autoIncrement()
                    title: String
                    author: Author
                }
            }
            "#,
            "optional-path.exo".to_string(),
        )
        .await
        .expect("Failed to build subsystem");

        let (_, library_entity) = subsystem
            .core_subsystem
            .entity_types
            .iter()
            .find(|(_, entity)| entity.name == "Library")
            .expect("Library entity not found");

        let featured_books_field = library_entity
            .field_by_name("featuredBooks")
            .expect("featuredBooks field missing");

        if let PostgresRelation::Transitive(transitive) = &featured_books_field.relation {
            assert_eq!(transitive.steps.len(), 2);
            assert!(
                transitive.steps[0].is_optional,
                "Expected first step to be optional"
            );
            assert!(matches!(
                transitive.steps[1].cardinality,
                RelationCardinality::Unbounded
            ));
        } else {
            panic!("Expected transitive relation");
        }
    }

    #[tokio::test]
    async fn relation_path_rejects_transitive_segments() {
        let result = builder::build_system_from_str(
            r#"
            @postgres
            module InvalidTransitiveSegment {
                @access(true)
                type Node {
                    @pk id: Int = autoIncrement()
                    @manyToOne
                    parent: Node?
                    @relation("parent")
                    children: Set<Node>
                    @relationPath("parent.parent")
                    grandparent: Node?
                    @relationPath("grandparent.children")
                    cousins: Set<Node>
                }
            }
            "#,
            "invalid-transitive-segment.exo".to_string(),
            vec![Box::new(
                postgres_builder::PostgresSubsystemBuilder::default(),
            )],
            core_model_builder::plugin::BuildMode::Build,
        )
        .await;

        let err = result
            .expect_err("Schema referencing a transitive relation in a relationPath should fail");
        let message = format!("{err}");
        assert!(
            message.contains("references transitive relation"),
            "Unexpected error message: {message}"
        );
    }
}
