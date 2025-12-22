// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Build the reference input type (used to refer to an entity by its pk)

use core_model::{
    access::AccessPredicateExpression,
    mapped_arena::{MappedArena, SerializableSlabIndex},
};
use core_model_builder::error::ModelBuildingError;
use postgres_graphql_model::types::MutationType;

use postgres_core_model::{
    relation::PostgresRelation,
    types::{EntityRepresentation, EntityType, PostgresField},
};

use crate::utils::{MutationTypeKind, to_mutation_type};

use super::{builder::Builder, naming::ToPostgresTypeNames, system_builder::SystemContextBuilding};
use postgres_core_builder::resolved_type::{ResolvedCompositeType, ResolvedType};

pub struct ReferenceInputTypeBuilder;

impl Builder for ReferenceInputTypeBuilder {
    fn type_names(
        &self,
        resolved_composite_type: &ResolvedCompositeType,
        _types: &MappedArena<ResolvedType>,
    ) -> Vec<String> {
        if !resolved_composite_type.access.creation_allowed()
            && !resolved_composite_type.access.update_allowed()
        {
            return vec![];
        }

        let has_pk = resolved_composite_type
            .fields
            .iter()
            .any(|field| field.is_pk);

        if !has_pk {
            return vec![];
        }

        vec![resolved_composite_type.reference_type()]
    }

    /// Expand the mutation input types as well as build the mutation
    fn build_expanded(
        &self,
        building: &mut SystemContextBuilding,
    ) -> Result<(), ModelBuildingError> {
        for (_, entity_type) in building.core_subsystem.entity_types.iter() {
            if entity_type.representation == EntityRepresentation::Json {
                continue;
            }

            if !entity_allows_reference_input(entity_type, building) {
                continue;
            }

            for (existing_id, expanded_type) in expanded_reference_types(entity_type, building) {
                building.mutation_types[existing_id] = expanded_type;
            }
        }

        Ok(())
    }

    fn needs_mutation_type(&self, composite_type: &ResolvedCompositeType) -> bool {
        composite_type.representation != EntityRepresentation::Json
    }
}

fn expanded_reference_types(
    entity_type: &EntityType,
    building: &SystemContextBuilding,
) -> Vec<(SerializableSlabIndex<MutationType>, MutationType)> {
    if !entity_allows_reference_input(entity_type, building) {
        return vec![];
    }

    let reference_type_fields: Vec<PostgresField<MutationType>> = entity_type
        .fields
        .iter()
        .flat_map(|field| match &field.relation {
            PostgresRelation::Scalar { is_pk: true, .. } => Some(PostgresField {
                name: field.name.clone(),
                typ: to_mutation_type(&field.typ, MutationTypeKind::Reference, building),
                access: field.access.clone(),
                relation: field.relation.clone(),
                default_value: field.default_value.clone(),
                readonly: field.readonly,
                type_validation: None,
                doc_comments: None,
            }),
            PostgresRelation::ManyToOne { is_pk: true, .. } => Some(PostgresField {
                name: field.name.clone(),
                typ: to_mutation_type(&field.typ, MutationTypeKind::Reference, building),
                access: field.access.clone(),
                relation: field.relation.clone(),
                default_value: field.default_value.clone(),
                readonly: field.readonly,
                type_validation: None,
                doc_comments: None,
            }),
            _ => None,
        })
        .collect();

    if reference_type_fields.is_empty() {
        return vec![];
    }

    let existing_type_name = entity_type.reference_type();

    let existing_type_id = building.mutation_types.get_id(&existing_type_name).unwrap();

    vec![(
        existing_type_id,
        MutationType {
            name: existing_type_name,
            fields: reference_type_fields,
            entity_id: building
                .core_subsystem
                .entity_types
                .get_id(&entity_type.name)
                .unwrap(),
            database_access: None,
            doc_comments: entity_type.doc_comments.clone(),
        },
    )]
}

fn entity_allows_reference_input(
    entity_type: &EntityType,
    building: &SystemContextBuilding,
) -> bool {
    let update_precheck_false = {
        let guard = building
            .core_subsystem
            .precheck_access_expressions
            .lock()
            .unwrap();
        matches!(
            guard[entity_type.access.update.precheck],
            AccessPredicateExpression::BooleanLiteral(false)
        )
    };

    let update_database_false = {
        let guard = building
            .core_subsystem
            .database_access_expressions
            .lock()
            .unwrap();
        matches!(
            guard[entity_type.access.update.database],
            AccessPredicateExpression::BooleanLiteral(false)
        )
    };

    let creation_access_is_false = {
        let guard = building
            .core_subsystem
            .precheck_access_expressions
            .lock()
            .unwrap();
        matches!(
            guard[entity_type.access.creation.precheck],
            AccessPredicateExpression::BooleanLiteral(false)
        )
    };

    !(creation_access_is_false && update_precheck_false && update_database_false)
}
