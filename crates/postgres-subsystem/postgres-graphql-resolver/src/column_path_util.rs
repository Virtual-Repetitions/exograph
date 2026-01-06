// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use exo_sql::{ColumnPathLink, PhysicalColumnPath};

pub fn to_column_path(
    parent_column_path: &Option<PhysicalColumnPath>,
    next_column_path_link: &Option<ColumnPathLink>,
) -> Option<PhysicalColumnPath> {
    match parent_column_path {
        Some(parent_column_path) => match next_column_path_link {
            Some(next_column_path_link) => {
                let (base_path, trailing_leaf) = match parent_column_path.last_link() {
                    ColumnPathLink::Leaf(column_id) => {
                        (parent_column_path.without_last(), Some(*column_id))
                    }
                    _ => (Some(parent_column_path.clone()), None),
                };

                match base_path {
                    Some(path) => Some(path.push(next_column_path_link.clone())),
                    None => match (trailing_leaf, next_column_path_link) {
                        (_, ColumnPathLink::Relation(_)) => {
                            Some(PhysicalColumnPath::init(next_column_path_link.clone()))
                        }
                        (_, ColumnPathLink::Leaf(column_id)) => {
                            Some(PhysicalColumnPath::leaf(*column_id))
                        }
                    },
                }
            }
            None => Some(parent_column_path.clone()),
        },
        None => next_column_path_link
            .as_ref()
            .map(|next_column_path_link| PhysicalColumnPath::init(next_column_path_link.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exo_sql::{ColumnId, RelationColumnPair, TableId};
    use typed_generational_arena::Index;

    fn table_id(index: usize) -> TableId {
        Index::from_idx(index)
    }

    fn column_id(table_id: TableId, column_index: usize) -> ColumnId {
        ColumnId {
            table_id,
            column_index,
        }
    }

    #[test]
    fn replaces_trailing_leaf_before_appending_relation() {
        let table_a = table_id(0);
        let table_b = table_id(1);
        let table_c = table_id(2);

        let relation_ab = ColumnPathLink::relation(
            vec![RelationColumnPair {
                self_column_id: column_id(table_a, 0),
                foreign_column_id: column_id(table_b, 0),
            }],
            None,
        );

        let parent_path = PhysicalColumnPath::init(relation_ab.clone())
            .push(ColumnPathLink::Leaf(column_id(table_b, 1)));

        let relation_bc = ColumnPathLink::relation(
            vec![RelationColumnPair {
                self_column_id: column_id(table_b, 2),
                foreign_column_id: column_id(table_c, 0),
            }],
            None,
        );

        let combined = to_column_path(&Some(parent_path), &Some(relation_bc.clone())).unwrap();

        let expected = PhysicalColumnPath::init(relation_ab).push(relation_bc);

        assert_eq!(combined, expected);
    }
}
