use async_recursion::async_recursion;
use common::context::RequestContext;
use core_model::types::{BaseOperationReturnType, FieldType, OperationReturnType};
use core_resolver::{
    QueryResponse, QueryResponseBody, access_solver::AccessSolver,
    system_resolver::GraphQLSystemResolver, validation::field::ValidatedField,
};
use deno_graphql_resolver::{
    DenoSubsystemResolver, ExoCallbackProcessor, InterceptedOperationInfo,
};
use exo_deno::Arg;
use exo_sql::AbstractPredicate;
use serde_json::{Map as JsonMap, Value, map::Entry as JsonEntry};

use crate::resolver::PostgresSubsystemResolver;
use postgres_core_model::relation::PostgresRelation;
use postgres_core_model::types::{
    ComputedField, EntityType, PostgresField, PostgresFieldType, TypeIndex,
};
use postgres_core_resolver::postgres_execution_error::PostgresExecutionError;

use crate::computed_fields::serde_helpers::args_to_json;

mod serde_helpers {
    use common::value::Val;
    use indexmap::IndexMap;
    use postgres_core_resolver::postgres_execution_error::PostgresExecutionError;
    use serde_json::{Map as JsonMap, Value};

    pub fn args_to_json(
        arguments: &IndexMap<String, Val>,
    ) -> Result<Value, PostgresExecutionError> {
        let mut map = JsonMap::new();
        for (key, value) in arguments.iter() {
            let json_value: Value = value.clone().try_into().map_err(|_| {
                PostgresExecutionError::Generic(format!(
                    "Failed to convert argument '{}' to JSON",
                    key
                ))
            })?;
            map.insert(key.clone(), json_value);
        }
        Ok(Value::Object(map))
    }
}

pub async fn apply_computed_fields_to_body(
    body: &mut QueryResponseBody,
    return_type: &OperationReturnType<EntityType>,
    field: &ValidatedField,
    subsystem_resolver: &PostgresSubsystemResolver,
    system_resolver: &GraphQLSystemResolver,
    request_context: &RequestContext<'_>,
) -> Result<(), PostgresExecutionError> {
    let entity_type = return_type.typ(&subsystem_resolver.subsystem.core_subsystem.entity_types);

    if !needs_postprocess(entity_type, &field.subfields, subsystem_resolver) {
        return Ok(());
    }

    let json_str = match body {
        QueryResponseBody::Raw(Some(s)) => s,
        _ => return Ok(()),
    };

    let mut value: Value = serde_json::from_str(json_str).map_err(|e| {
        PostgresExecutionError::Generic(format!("Failed to parse query result JSON: {e}"))
    })?;

    process_value(
        &mut value,
        return_type,
        &field.subfields,
        subsystem_resolver,
        system_resolver,
        request_context,
    )
    .await?;

    let updated = serde_json::to_string(&value).map_err(|e| {
        PostgresExecutionError::Generic(format!("Failed to serialize query result JSON: {e}"))
    })?;

    *body = QueryResponseBody::Raw(Some(updated));

    Ok(())
}

fn selection_to_json(selection: &[ValidatedField]) -> Result<Value, PostgresExecutionError> {
    let mut fields = Vec::with_capacity(selection.len());
    for field in selection {
        fields.push(selection_field_to_json(field)?);
    }
    Ok(Value::Array(fields))
}

fn selection_field_to_json(field: &ValidatedField) -> Result<Value, PostgresExecutionError> {
    let mut map = JsonMap::new();
    map.insert("name".to_string(), Value::String(field.name.to_string()));
    map.insert("outputName".to_string(), Value::String(field.output_name()));

    if let Some(alias) = &field.alias {
        map.insert("alias".to_string(), Value::String(alias.to_string()));
    }

    if !field.arguments.is_empty() {
        map.insert("arguments".to_string(), args_to_json(&field.arguments)?);
    }

    if !field.subfields.is_empty() {
        map.insert("fields".to_string(), selection_to_json(&field.subfields)?);
    }

    Ok(Value::Object(map))
}

#[async_recursion]
async fn process_value(
    value: &mut Value,
    return_type: &OperationReturnType<EntityType>,
    selection: &[ValidatedField],
    subsystem_resolver: &PostgresSubsystemResolver,
    system_resolver: &GraphQLSystemResolver,
    request_context: &RequestContext<'_>,
) -> Result<(), PostgresExecutionError> {
    match return_type {
        OperationReturnType::Plain(_) => {
            let entity_type =
                return_type.typ(&subsystem_resolver.subsystem.core_subsystem.entity_types);
            process_entity(
                value,
                entity_type,
                selection,
                subsystem_resolver,
                system_resolver,
                request_context,
            )
            .await
        }
        OperationReturnType::Optional(inner) => {
            if value.is_null() {
                Ok(())
            } else {
                process_value(
                    value,
                    inner,
                    selection,
                    subsystem_resolver,
                    system_resolver,
                    request_context,
                )
                .await
            }
        }
        OperationReturnType::List(inner) => {
            if let Value::Array(items) = value {
                for item in items.iter_mut() {
                    process_value(
                        item,
                        inner,
                        selection,
                        subsystem_resolver,
                        system_resolver,
                        request_context,
                    )
                    .await?;
                }
                Ok(())
            } else if value.is_null() {
                Ok(())
            } else {
                Err(PostgresExecutionError::Generic(
                    "Expected array value for list return type".to_string(),
                ))
            }
        }
    }
}

#[async_recursion]
async fn process_entity(
    value: &mut Value,
    entity_type: &EntityType,
    selection: &[ValidatedField],
    subsystem_resolver: &PostgresSubsystemResolver,
    system_resolver: &GraphQLSystemResolver,
    request_context: &RequestContext<'_>,
) -> Result<(), PostgresExecutionError> {
    let obj = match value {
        Value::Object(map) => map,
        Value::Null => {
            return Ok(());
        }
        _ => {
            return Err(PostgresExecutionError::Generic(
                "Expected object when processing computed fields".to_string(),
            ));
        }
    };

    let mut projected_keys: Vec<String> = Vec::with_capacity(selection.len() + 1);

    for selection_field in selection {
        let field_name = &selection_field.name;

        if field_name == "__typename" {
            projected_keys.push(field_name.to_string());
            continue;
        }

        let output_name = selection_field.output_name();
        projected_keys.push(output_name.clone());

        let entity_field = match entity_type.field_by_name(field_name) {
            Some(field) => field,
            None => continue,
        };

        if !is_field_authorized(entity_field, subsystem_resolver, request_context).await? {
            continue;
        }

        if let PostgresRelation::Computed(computed) = &entity_field.relation {
            let dependency_placeholder = obj.get(&output_name).cloned();

            let mut parent_snapshot = Value::Object(obj.clone());
            if let Value::Object(ref mut parent_map) = parent_snapshot
                && let Some(Value::Object(dependency_values)) = dependency_placeholder.as_ref()
            {
                for (dep_name, dep_value) in dependency_values {
                    parent_map
                        .entry(dep_name.clone())
                        .or_insert(dep_value.clone());
                }
            }

            let computed_value = execute_computed_field(
                computed,
                &parent_snapshot,
                selection_field,
                subsystem_resolver,
                system_resolver,
                request_context,
            )
            .await?;

            let entry = obj.entry(output_name.clone());
            let entry_ref = match entry {
                JsonEntry::Vacant(vacant) => vacant.insert(Value::Null),
                JsonEntry::Occupied(occupied) => occupied.into_mut(),
            };

            *entry_ref = computed_value;

            if !selection_field.subfields.is_empty()
                && let Some(nested_return_type) = field_operation_return_type(&entity_field.typ)
            {
                process_value(
                    entry_ref,
                    &nested_return_type,
                    &selection_field.subfields,
                    subsystem_resolver,
                    system_resolver,
                    request_context,
                )
                .await?;
            }
        } else if !selection_field.subfields.is_empty()
            && let Some(field_value) = obj.get_mut(&output_name)
            && let Some(nested_return_type) = field_operation_return_type(&entity_field.typ)
        {
            process_value(
                field_value,
                &nested_return_type,
                &selection_field.subfields,
                subsystem_resolver,
                system_resolver,
                request_context,
            )
            .await?;
        }
    }

    if entity_type.representation.is_json_like() {
        obj.retain(|key, _| projected_keys.iter().any(|allowed| allowed == key));
    }

    Ok(())
}

async fn execute_computed_field(
    computed: &ComputedField,
    parent_snapshot: &Value,
    selection_field: &ValidatedField,
    subsystem_resolver: &PostgresSubsystemResolver,
    system_resolver: &GraphQLSystemResolver,
    request_context: &RequestContext<'_>,
) -> Result<Value, PostgresExecutionError> {
    let subsystem_id = computed.subsystem.as_str();
    if subsystem_id != "deno" {
        return Err(PostgresExecutionError::Generic(format!(
            "Unsupported computed field subsystem '{}'",
            subsystem_id
        )));
    }

    let deno_resolver_arc = system_resolver
        .find_subsystem_resolver(subsystem_id)
        .ok_or_else(|| {
            PostgresExecutionError::Generic(format!(
                "Subsystem '{}' is not available",
                subsystem_id
            ))
        })?;

    let deno_resolver = deno_resolver_arc
        .as_ref()
        .as_any()
        .downcast_ref::<DenoSubsystemResolver>()
        .ok_or_else(|| {
            PostgresExecutionError::Generic(format!(
                "Subsystem '{}' is not a Deno subsystem",
                subsystem_id
            ))
        })?;

    let script = &subsystem_resolver.subsystem.core_subsystem.computed_scripts[computed.script_id];

    let script_defn: exo_deno::deno_executor_pool::DenoScriptDefn =
        serde_json::from_slice(&script.definition).map_err(|e| {
            PostgresExecutionError::Generic(format!(
                "Failed to deserialize computed script '{}': {e}",
                script.path
            ))
        })?;

    let args_value = args_to_json(&selection_field.arguments)?;
    let selection_value = selection_to_json(&selection_field.subfields)?;

    let exograph_execute_query =
        core_resolver::exograph_execute_query!(system_resolver, request_context);
    let callback_processor = ExoCallbackProcessor {
        exograph_execute_query,
        exograph_proceed: None,
    };

    // Pass four arguments: parent, args, selection, exograph
    // The 4th argument (Exograph) is required for computed resolvers
    let arg_sequence = vec![
        Arg::Serde(parent_snapshot.clone()),
        Arg::Serde(args_value),
        Arg::Serde(selection_value),
        Arg::Shim("Exograph".to_string()), // Injected Exograph client
    ];

    deno_resolver
        .executor
        .execute_and_get_r(
            &script.path,
            script_defn,
            &computed.function_name,
            arg_sequence,
            Option::<InterceptedOperationInfo>::None,
            callback_processor,
        )
        .await
        .map(|(value, _)| value)
        .map_err(|e| {
            PostgresExecutionError::Generic(format!(
                "Failed to evaluate computed field '{}': {e}",
                selection_field.name
            ))
        })
}

async fn is_field_authorized(
    field: &PostgresField<EntityType>,
    subsystem_resolver: &PostgresSubsystemResolver,
    request_context: &RequestContext<'_>,
) -> Result<bool, PostgresExecutionError> {
    let predicate = subsystem_resolver
        .subsystem
        .core_subsystem
        .solve(
            request_context,
            None,
            &subsystem_resolver
                .subsystem
                .core_subsystem
                .database_access_expressions[field.access.read],
        )
        .await?
        .map(|p| p.0)
        .resolve();

    Ok(predicate != AbstractPredicate::False)
}

fn needs_postprocess(
    entity_type: &EntityType,
    selection: &[ValidatedField],
    subsystem_resolver: &PostgresSubsystemResolver,
) -> bool {
    if entity_type.representation.is_json_like() && !selection.is_empty() {
        return true;
    }

    for selection_field in selection {
        if let Some(entity_field) = entity_type.field_by_name(&selection_field.name) {
            match &entity_field.relation {
                PostgresRelation::Computed(_) => return true,
                PostgresRelation::Embedded => {
                    if !selection_field.subfields.is_empty() {
                        return true;
                    }
                }
                PostgresRelation::ManyToOne { .. } | PostgresRelation::OneToMany(_) => {
                    if let Some(nested_return_type) = field_operation_return_type(&entity_field.typ)
                    {
                        let nested_entity = nested_return_type
                            .typ(&subsystem_resolver.subsystem.core_subsystem.entity_types);
                        if needs_postprocess(
                            nested_entity,
                            &selection_field.subfields,
                            subsystem_resolver,
                        ) {
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    false
}

fn field_operation_return_type(
    field_type: &FieldType<PostgresFieldType<EntityType>>,
) -> Option<OperationReturnType<EntityType>> {
    match field_type {
        FieldType::Plain(inner) => match inner.type_id {
            TypeIndex::Composite(type_id) => {
                Some(OperationReturnType::Plain(BaseOperationReturnType {
                    associated_type_id: type_id,
                    type_name: inner.type_name.clone(),
                }))
            }
            TypeIndex::Primitive(_) => None,
        },
        FieldType::Optional(inner) => field_operation_return_type(inner)
            .map(|inner_type| OperationReturnType::Optional(Box::new(inner_type))),
        FieldType::List(inner) => field_operation_return_type(inner)
            .map(|inner_type| OperationReturnType::List(Box::new(inner_type))),
    }
}
