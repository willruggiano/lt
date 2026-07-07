//! Maps each fragment field to its storage role by resolving the field's
//! schema type ([`schema_model`](crate::schema_model)) and nullability
//! ([`selection_model`](crate::selection_model)).
//!
//! Fails closed (build-time panic, per this crate's build-script convention)
//! on any selection shape or schema reference the generator does not model.

use std::collections::BTreeSet;

use crate::schema_model::{Object, Schema, TypeKind};
use crate::selection_model::{Fragment, FragmentField};

/// The storage role a fragment field maps to.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldRole {
    /// The `id` field: primary key / upsert key (the ENG-84 `ON UPDATE
    /// CASCADE` identity anchor).
    PrimaryKey,
    /// A scalar or enum column.
    ScalarColumn {
        column: String,
        not_null: bool,
        schema_type: String,
    },
    /// A foreign key to a `Node`-implementing object's table.
    ForeignKey {
        column: String,
        target_type: String,
        not_null: bool,
    },
    /// A `*Connection` field, modeled as a junction table.
    Junction {
        gql_field: String,
        connection_type: String,
    },
}

/// The schema and generated-tables set every field in a fragment is
/// classified against, bundled to keep [`classify_field`] under the
/// project's parameter-count limit (`clippy.toml`: `too-many-arguments-threshold`).
struct ClassifyEnv<'a> {
    schema: &'a Schema,
    generated_types: &'a BTreeSet<&'a str>,
}

/// Classify every field of `fragment` against `schema`.
///
/// `generated_types` is the set of GraphQL object type names that have a
/// generated table -- the `graphql_type` of every extracted fragment. A
/// field classified as a foreign key whose target is outside this set
/// panics.
pub fn classify_fragment(
    fragment: &Fragment,
    schema: &Schema,
    generated_types: &BTreeSet<&str>,
) -> Vec<FieldRole> {
    let object = schema
        .object(&fragment.graphql_type)
        .unwrap_or_else(|| panic!("schema has no object type `{}`", fragment.graphql_type));
    let env = ClassifyEnv {
        schema,
        generated_types,
    };

    fragment
        .fields
        .iter()
        .map(|field| classify_field(field, &fragment.graphql_type, &object, &env))
        .collect()
}

fn classify_field(
    field: &FragmentField,
    graphql_type: &str,
    object: &Object<'_>,
    env: &ClassifyEnv<'_>,
) -> FieldRole {
    if let Some(attr) = field
        .cynic_attrs
        .iter()
        .find(|attr| attr.as_str() != "rename")
    {
        panic!(
            "field `{}` on `{graphql_type}` carries unsupported cynic attribute `#[cynic({attr})]`",
            field.rust_ident
        );
    }

    if field.gql_field == "id" {
        return FieldRole::PrimaryKey;
    }

    let schema_type = object
        .fields()
        .find_map(|(name, ty)| (name == field.gql_field).then(|| ty.to_string()))
        .unwrap_or_else(|| {
            panic!(
                "field `{}` (`{}`) not found on schema type `{graphql_type}`",
                field.rust_ident, field.gql_field
            )
        });

    if schema_type.ends_with("Connection") {
        return FieldRole::Junction {
            gql_field: field.gql_field.clone(),
            connection_type: schema_type,
        };
    }

    match env.schema.type_kind(&schema_type) {
        Some(TypeKind::Scalar | TypeKind::Enum) => FieldRole::ScalarColumn {
            column: to_snake_case(&field.gql_field),
            not_null: !field.nullable,
            schema_type,
        },
        Some(TypeKind::Object {
            implements_node: true,
        }) => {
            assert!(
                env.generated_types.contains(schema_type.as_str()),
                "field `{}` targets `{schema_type}`, which has no generated table",
                field.rust_ident
            );
            FieldRole::ForeignKey {
                column: format!("{}_id", to_snake_case(&field.gql_field)),
                target_type: schema_type,
                not_null: !field.nullable,
            }
        }
        Some(TypeKind::Object {
            implements_node: false,
        }) => panic!(
            "field `{}` targets `{schema_type}`, an object that does not implement Node",
            field.rust_ident
        ),
        None => panic!("schema has no scalar/enum/object type `{schema_type}`"),
    }
}

/// Convert `camelCase` to `snake_case` -- inverse of
/// [`crate::selection_model::to_camel_case`].
pub fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{FieldRole, classify_fragment, to_snake_case};
    use crate::schema_model::Schema;
    use crate::selection_model::parse_fragments;

    const SDL: &str = r"
        interface Node {
            id: ID!
        }

        type WorkflowState implements Node {
            id: ID!
        }

        type User implements Node {
            id: ID!
        }

        type IssueLabelConnection {
            nodes: [String!]!
        }

        type Issue implements Node {
            id: ID!
            state: WorkflowState!
            labels: IssueLabelConnection!
            priorityLabel: String!
            assignee: User
        }
    ";

    const FRAGMENT: &str = r#"
        #[derive(cynic::QueryFragment)]
        #[cynic(graphql_type = "Issue")]
        pub struct Issue {
            pub id: cynic::Id,
            pub state: WorkflowState,
            pub labels: IssueLabelConnection,
            pub priority_label: String,
            pub assignee: Option<User>,
        }
    "#;

    fn issue_fragment(src: &str) -> crate::selection_model::Fragment {
        parse_fragments(src)
            .expect("fragment source parses")
            .into_iter()
            .find(|f| f.rust_name == "Issue")
            .expect("Issue fragment present")
    }

    fn role<'a>(
        roles: &'a [FieldRole],
        fragment: &crate::selection_model::Fragment,
        rust_ident: &str,
    ) -> &'a FieldRole {
        let index = fragment
            .fields
            .iter()
            .position(|f| f.rust_ident == rust_ident)
            .unwrap_or_else(|| panic!("missing field {rust_ident}"));
        &roles[index]
    }

    #[test]
    fn classify_fragment_maps_each_role() {
        let schema = Schema::parse(SDL).expect("SDL parses");
        let fragment = issue_fragment(FRAGMENT);
        let generated_types = BTreeSet::from(["Issue", "WorkflowState", "User"]);

        let roles = classify_fragment(&fragment, &schema, &generated_types);

        assert_eq!(*role(&roles, &fragment, "id"), FieldRole::PrimaryKey);

        assert_eq!(
            *role(&roles, &fragment, "state"),
            FieldRole::ForeignKey {
                column: "state_id".to_string(),
                target_type: "WorkflowState".to_string(),
                not_null: true,
            }
        );

        assert_eq!(
            *role(&roles, &fragment, "labels"),
            FieldRole::Junction {
                gql_field: "labels".to_string(),
                connection_type: "IssueLabelConnection".to_string(),
            }
        );

        assert_eq!(
            *role(&roles, &fragment, "priority_label"),
            FieldRole::ScalarColumn {
                column: "priority_label".to_string(),
                not_null: true,
                schema_type: "String".to_string(),
            }
        );

        assert_eq!(
            *role(&roles, &fragment, "assignee"),
            FieldRole::ForeignKey {
                column: "assignee_id".to_string(),
                target_type: "User".to_string(),
                not_null: false,
            }
        );
    }

    #[test]
    #[should_panic(expected = "cynic(spread)")]
    fn classify_fragment_panics_on_unsupported_cynic_attribute() {
        const SPREAD_FRAGMENT: &str = r#"
            #[derive(cynic::QueryFragment)]
            #[cynic(graphql_type = "Issue")]
            pub struct Issue {
                pub id: cynic::Id,
                #[cynic(spread)]
                pub state: WorkflowState,
            }
        "#;
        let schema = Schema::parse(SDL).expect("SDL parses");
        let fragment = issue_fragment(SPREAD_FRAGMENT);
        let generated_types = BTreeSet::from(["Issue", "WorkflowState"]);

        classify_fragment(&fragment, &schema, &generated_types);
    }

    #[test]
    #[should_panic(expected = "has no generated table")]
    fn classify_fragment_panics_on_node_fk_without_generated_table() {
        let schema = Schema::parse(SDL).expect("SDL parses");
        let fragment = issue_fragment(FRAGMENT);
        // `WorkflowState` is missing, so the `state` foreign key has no target table.
        let generated_types = BTreeSet::from(["Issue", "User"]);

        classify_fragment(&fragment, &schema, &generated_types);
    }

    #[test]
    fn to_snake_case_converts_camel_case() {
        assert_eq!(to_snake_case("priorityLabel"), "priority_label");
        assert_eq!(to_snake_case("createdAt"), "created_at");
        assert_eq!(to_snake_case("title"), "title");
    }
}
