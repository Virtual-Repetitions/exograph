// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use tracing::instrument;

use crate::{
    AbstractOrderBy, AbstractPredicate, ColumnId, Database,
    asql::{
        abstract_operation::AbstractOperation, delete::AbstractDelete, insert::AbstractInsert,
        select::AbstractSelect, update::AbstractUpdate,
    },
    sql::{
        order::OrderBy,
        predicate::ConcretePredicate,
        select::Select,
        transaction::{TransactionScript, TransactionStepId},
    },
};

use super::pg::{Postgres, selection_level::SelectionLevel};

/// Transform an abstract operation into a transaction script
pub trait OperationTransformer {
    fn to_transaction_script<'a>(
        &self,
        database: &'a Database,
        abstract_operation: AbstractOperation,
    ) -> TransactionScript<'a>;
}

impl OperationTransformer for Postgres {
    fn to_transaction_script<'a>(
        &self,
        database: &'a Database,
        abstract_operation: AbstractOperation,
    ) -> TransactionScript<'a> {
        match abstract_operation {
            AbstractOperation::Select(select) => {
                SelectTransformer::to_transaction_script(self, select, database)
            }
            AbstractOperation::Delete(delete) => {
                DeleteTransformer::to_transaction_script(self, delete, database)
            }
            AbstractOperation::Insert(insert) => {
                InsertTransformer::to_transaction_script(self, insert, None, database)
            }
            AbstractOperation::Update(update) => {
                UpdateTransformer::to_transaction_script(self, update, database)
            }
        }
    }
}

pub trait SelectTransformer {
    fn to_select(&self, abstract_select: AbstractSelect, database: &Database) -> Select;

    fn to_transaction_script<'a>(
        &self,
        abstract_select: AbstractSelect,
        database: &'a Database,
    ) -> TransactionScript<'a>;
}

pub trait DeleteTransformer {
    #[instrument(
        name = "DeleteTransformer::to_transaction_script for Postgres"
        skip(self, database)
        )]
    fn to_transaction_script<'a>(
        &self,
        abstract_delete: AbstractDelete,
        database: &'a Database,
    ) -> TransactionScript<'a> {
        let mut transaction_script = TransactionScript::default();

        self.update_transaction_script(abstract_delete, database, &mut transaction_script);

        transaction_script
    }

    fn update_transaction_script<'a>(
        &self,
        abstract_delete: AbstractDelete,
        database: &'a Database,
        transaction_script: &mut TransactionScript<'a>,
    );
}

pub trait InsertTransformer {
    #[instrument(
        name = "InsertTransformer::to_transaction_script for Postgres"
        skip(self, parent_step, database)
        )]
    fn to_transaction_script<'a>(
        &self,
        abstract_insert: AbstractInsert,
        parent_step: Option<(TransactionStepId, Vec<ColumnId>)>,
        database: &'a Database,
    ) -> TransactionScript<'a> {
        let mut transaction_script = TransactionScript::default();

        self.update_transaction_script(
            abstract_insert,
            parent_step,
            database,
            &mut transaction_script,
        );

        transaction_script
    }

    fn update_transaction_script<'a>(
        &self,
        abstract_insert: AbstractInsert,
        parent_step: Option<(TransactionStepId, Vec<ColumnId>)>,
        database: &'a Database,
        transaction_script: &mut TransactionScript<'a>,
    );
}

pub trait UpdateTransformer {
    #[instrument(
        name = "UpdateTransformer::to_transaction_script for Postgres"
        skip(self, database)
        )]
    fn to_transaction_script<'a>(
        &self,
        abstract_update: AbstractUpdate,
        database: &'a Database,
    ) -> TransactionScript<'a> {
        let mut transaction_script = TransactionScript::default();

        self.update_transaction_script(abstract_update, database, &mut transaction_script);

        transaction_script
    }

    fn update_transaction_script<'a>(
        &self,
        abstract_update: AbstractUpdate,
        database: &'a Database,
        transaction_script: &mut TransactionScript<'a>,
    );
}

pub trait PredicateTransformer {
    /// Transform an abstract predicate into a concrete predicate
    ///
    /// # Arguments
    /// * `predicate` - The predicate to transform
    /// * `selection_level` - The selection level of that led to this predicate (through subselects)
    /// * `tables_supplied` - Whether the tables are already in context. If they are, the predicate can simply use the table.column syntax.
    ///   If they are not, the predicate will need to bring in the tables being referred to.
    /// * `database` - The database
    fn to_predicate(
        &self,
        predicate: &AbstractPredicate,
        selection_level: &SelectionLevel,
        assume_tables_in_context: bool,
        database: &Database,
    ) -> ConcretePredicate;
}

pub trait OrderByTransformer {
    fn to_order_by(
        &self,
        order_by: &AbstractOrderBy,
        selection_level: &SelectionLevel,
        database: &Database,
    ) -> OrderBy;
}

#[cfg(test)]
mod tests {
    use multiplatform_test::multiplatform_test;

    use crate::{
        AbstractPredicate, AbstractSelect, ColumnPath, PhysicalColumnPath,
        asql::{
            delete::AbstractDelete,
            insert::{AbstractInsert, ColumnValuePair, InsertionElement, InsertionRow},
            selection::{AliasedSelectionElement, Selection, SelectionElement},
            update::AbstractUpdate,
        },
        sql::{
            SQLParamContainer, column::Column, predicate::Predicate, transaction::TransactionStep,
        },
        transform::{pg::Postgres, test_util::TestSetup},
    };

    use super::*;

    fn venue_selection(setup: &TestSetup) -> AbstractSelect {
        AbstractSelect {
            table_id: setup.venues_table,
            selection: Selection::Seq(vec![AliasedSelectionElement::new(
                "id".to_string(),
                SelectionElement::Physical(setup.venues_id_column),
            )]),
            predicate: Predicate::True,
            order_by: None,
            offset: None,
            limit: None,
        }
    }

    fn name_is(setup: &TestSetup, name: &str) -> AbstractPredicate {
        AbstractPredicate::Eq(
            ColumnPath::Physical(PhysicalColumnPath::leaf(setup.venues_name_column)),
            ColumnPath::Param(SQLParamContainer::string(name.to_string())),
        )
    }

    // The transaction holder defers `BEGIN` until the first script that is not read-only, so a
    // query must classify as read-only and every kind of write (and its prechecks) must not.

    #[multiplatform_test]
    fn select_script_is_read_only() {
        TestSetup::with_setup(|setup| {
            let script = OperationTransformer::to_transaction_script(
                &Postgres {},
                &setup.database,
                AbstractOperation::Select(AbstractSelect {
                    predicate: name_is(&setup, "v1"),
                    ..venue_selection(&setup)
                }),
            );
            assert!(script.is_read_only());
            assert!(!script.needs_transaction());
        });
    }

    #[multiplatform_test]
    fn insert_script_is_not_read_only() {
        TestSetup::with_setup(|setup| {
            let script = OperationTransformer::to_transaction_script(
                &Postgres {},
                &setup.database,
                AbstractOperation::Insert(AbstractInsert {
                    table_id: setup.venues_table,
                    rows: vec![InsertionRow {
                        elems: vec![InsertionElement::SelfInsert(ColumnValuePair::new(
                            setup.venues_name_column,
                            Column::Param(SQLParamContainer::string("v1".to_string())),
                        ))],
                    }],
                    selection: venue_selection(&setup),
                    precheck_predicates: vec![],
                }),
            );
            assert!(!script.is_read_only());
        });
    }

    #[multiplatform_test]
    fn update_script_with_precheck_is_not_read_only() {
        TestSetup::with_setup(|setup| {
            let script = OperationTransformer::to_transaction_script(
                &Postgres {},
                &setup.database,
                AbstractOperation::Update(AbstractUpdate {
                    table_id: setup.venues_table,
                    predicate: name_is(&setup, "v1"),
                    column_values: vec![(
                        setup.venues_name_column,
                        Column::Param(SQLParamContainer::string("v2".to_string())),
                    )],
                    nested_updates: vec![],
                    nested_inserts: vec![],
                    nested_deletes: vec![],
                    selection: venue_selection(&setup),
                    precheck_predicates: vec![name_is(&setup, "v1")],
                }),
            );
            assert!(!script.is_read_only());
        });
    }

    #[multiplatform_test]
    fn delete_script_is_not_read_only() {
        TestSetup::with_setup(|setup| {
            let script = OperationTransformer::to_transaction_script(
                &Postgres {},
                &setup.database,
                AbstractOperation::Delete(AbstractDelete {
                    table_id: setup.venues_table,
                    selection: venue_selection(&setup),
                    predicate: name_is(&setup, "v1"),
                    precheck_predicates: vec![],
                }),
            );
            assert!(!script.is_read_only());
        });
    }

    #[multiplatform_test]
    fn precheck_step_is_not_read_only() {
        // A precheck is a SELECT, but it only ever guards a write, so it must not count as a read.
        TestSetup::with_setup(|setup| {
            let precheck = Postgres {}.to_select(venue_selection(&setup), &setup.database);
            let mut script = TransactionScript::default();
            script.add_step(TransactionStep::Precheck(precheck));
            assert!(!script.is_read_only());
        });
    }
}
