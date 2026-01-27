// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::collections::HashMap;

use codemap_diagnostic::{Diagnostic, Level, SpanLabel, SpanStyle};
use core_model::mapped_arena::MappedArena;
use core_model_builder::typechecker::{Typed, annotation::AnnotationSpec};

use crate::ast::ast_types::{
    AstExpr, FieldSelection, FieldSelectionElement, LogicalOp, RelationalOp, Untyped,
};

use super::{Scope, Type, TypecheckFrom};

impl TypecheckFrom<AstExpr<Untyped>> for AstExpr<Typed> {
    fn shallow(untyped: &AstExpr<Untyped>) -> AstExpr<Typed> {
        match untyped {
            AstExpr::FieldSelection(select) => {
                AstExpr::FieldSelection(FieldSelection::shallow(select))
            }
            AstExpr::LogicalOp(logic) => AstExpr::LogicalOp(LogicalOp::shallow(logic)),
            AstExpr::RelationalOp(relation) => {
                AstExpr::RelationalOp(RelationalOp::shallow(relation))
            }
            AstExpr::EnumLiteral(enum_name, value, span, _) => {
                AstExpr::EnumLiteral(enum_name.clone(), value.clone(), *span, Type::Defer)
            }
            AstExpr::StringLiteral(v, s) => AstExpr::StringLiteral(v.clone(), *s),
            AstExpr::BooleanLiteral(v, s) => AstExpr::BooleanLiteral(*v, *s),
            AstExpr::NumberLiteral(v, s) => AstExpr::NumberLiteral(v.clone(), *s),
            AstExpr::StringList(v, s) => AstExpr::StringList(v.clone(), s.clone()),
            AstExpr::NullLiteral(s) => AstExpr::NullLiteral(*s),
            AstExpr::ObjectLiteral(m, s) => AstExpr::ObjectLiteral(
                m.iter()
                    .map(|(k, v)| (k.clone(), AstExpr::shallow(v)))
                    .collect(),
                *s,
            ),
        }
    }

    fn pass(
        &mut self,
        type_env: &MappedArena<Type>,
        annotation_env: &HashMap<String, AnnotationSpec>,
        scope: &Scope,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        match self {
            AstExpr::FieldSelection(select) => {
                if let Some((enum_name, value, span)) = enum_literal_parts(select) {
                    if let Some(typ) = resolve_enum_literal(
                        &enum_name,
                        &value,
                        span,
                        type_env,
                        scope,
                        errors,
                    ) {
                        *self = AstExpr::EnumLiteral(enum_name, value, span, typ);
                        true
                    } else {
                        select.pass(type_env, annotation_env, scope, errors)
                    }
                } else {
                    select.pass(type_env, annotation_env, scope, errors)
                }
            }
            AstExpr::LogicalOp(logic) => logic.pass(type_env, annotation_env, scope, errors),
            AstExpr::RelationalOp(relation) => {
                relation.pass(type_env, annotation_env, scope, errors)
            }
            AstExpr::EnumLiteral(enum_name, value, span, typ) => {
                if typ.is_incomplete() {
                    if let Some(resolved) =
                        resolve_enum_literal(enum_name, value, *span, type_env, scope, errors)
                    {
                        *typ = resolved;
                        true
                    } else {
                        *typ = Type::Error;
                        false
                    }
                } else {
                    false
                }
            }
            AstExpr::StringList(_, _)
            | AstExpr::StringLiteral(_, _)
            | AstExpr::BooleanLiteral(_, _)
            | AstExpr::NumberLiteral(_, _)
            | AstExpr::NullLiteral(_)
            | AstExpr::ObjectLiteral(_, _) => false,
        }
    }
}

fn enum_literal_parts(
    selection: &FieldSelection<Typed>,
) -> Option<(String, String, codemap::Span)> {
    match selection {
        FieldSelection::Select(prefix, elem, span, _) => match (prefix.as_ref(), elem) {
            (FieldSelection::Single(prefix_elem, _), FieldSelectionElement::Identifier(value, _, _)) => {
                if let FieldSelectionElement::Identifier(enum_name, _, _) = prefix_elem {
                    Some((enum_name.clone(), value.clone(), *span))
                } else {
                    None
                }
            }
            _ => None,
        },
        _ => None,
    }
}

fn resolve_enum_literal(
    enum_name: &str,
    value: &str,
    span: codemap::Span,
    type_env: &MappedArena<Type>,
    scope: &Scope,
    errors: &mut Vec<Diagnostic>,
) -> Option<Type> {
    if scope.get_type(enum_name).is_some() {
        return None;
    }

    let is_context = type_env
        .get_by_key(enum_name)
        .and_then(|t| match t {
            Type::Composite(c) if c.kind == crate::ast::ast_types::AstModelKind::Context => {
                Some(c)
            }
            _ => None,
        })
        .is_some();

    if is_context {
        return None;
    }

    let enum_type = type_env.get_by_key(enum_name).and_then(|t| match t {
        Type::Enum(e) => Some(e.clone()),
        _ => None,
    });

    let enum_type = match enum_type {
        Some(enum_type) => enum_type,
        None => return None,
    };

    if enum_type.fields.iter().any(|f| f.name == value) {
        Some(Type::Enum(enum_type))
    } else {
        errors.push(Diagnostic {
            level: Level::Error,
            message: format!(
                "Unknown variant '{value}' for enum '{enum_name}'"
            ),
            code: Some("C000".to_string()),
            spans: vec![SpanLabel {
                span,
                style: SpanStyle::Primary,
                label: Some("unknown enum variant".to_string()),
            }],
        });
        Some(Type::Error)
    }
}
