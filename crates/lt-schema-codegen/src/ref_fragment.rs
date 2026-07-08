//! Generates the sparse `*Ref` fragment for a type: its NOT NULL scalar
//! columns plus the FK id of each NOT NULL relation.
//!
//! `TRef` is `T`'s non-`Option` scalar fields plus the FK id of each
//! non-`Option` relation (a `<field>_id` selecting only the target's `id`).
//! `Option` fields and connection/junction fields are excluded. This ref set
//! doubles as the table's NOT NULL column declaration.
//!
//! **Termination invariant**: a relation is represented only by its FK id --
//! the target's own `id` field -- never by inlining the target's `*Ref`.
//! [`ref_fragment`] therefore inspects only the fields of the one fragment it
//! is given and never looks up or recurses into another fragment, so it
//! terminates by construction for self-referential (`Issue.parent -> Issue`)
//! and mutually-recursive types alike: there is no nested-ref recursion to
//! diverge.

use std::collections::BTreeSet;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::classify::{FieldRole, classify_fragment};
use crate::schema_model::Schema;
use crate::selection_model::Fragment;

/// A NOT NULL scalar (or enum) column of a `*Ref` fragment.
#[derive(Debug, Clone, PartialEq)]
pub struct RefScalarMember {
    pub rust_ident: String,
    pub gql_field: String,
    pub column: String,
    pub rust_base_type: String,
    pub list: bool,
}

/// A NOT NULL foreign-key column of a `*Ref` fragment, selecting only the
/// target's `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct RefForeignKeyMember {
    pub rust_ident: String,
    pub gql_field: String,
    pub column: String,
    pub target_type: String,
}

/// The sparse `*Ref` fragment for a type: its NOT NULL scalar and FK members.
#[derive(Debug, Clone, PartialEq)]
pub struct RefFragment {
    pub type_name: String,
    pub scalar_members: Vec<RefScalarMember>,
    pub fk_members: Vec<RefForeignKeyMember>,
}

/// Derive `fragment`'s `*Ref` member set: its `id`, its NOT NULL scalar
/// columns, and the FK id of each NOT NULL relation. Nullable fields and
/// junction (`*Connection`) fields are dropped.
pub fn ref_fragment(
    fragment: &Fragment,
    schema: &Schema,
    generated_types: &BTreeSet<&str>,
) -> RefFragment {
    let roles = classify_fragment(fragment, schema, generated_types);

    let mut scalar_members = Vec::new();
    let mut fk_members = Vec::new();

    for (field, role) in fragment.fields.iter().zip(roles.iter()) {
        match role {
            FieldRole::PrimaryKey => scalar_members.push(RefScalarMember {
                rust_ident: field.rust_ident.clone(),
                gql_field: field.gql_field.clone(),
                column: "id".to_string(),
                rust_base_type: field.base_type.clone(),
                list: false,
            }),
            FieldRole::ScalarColumn {
                column,
                not_null: true,
                schema_type: _,
            } => scalar_members.push(RefScalarMember {
                rust_ident: field.rust_ident.clone(),
                gql_field: field.gql_field.clone(),
                column: column.clone(),
                rust_base_type: field.base_type.clone(),
                list: field.list,
            }),
            FieldRole::ForeignKey {
                column,
                target_type,
                not_null: true,
            } => fk_members.push(RefForeignKeyMember {
                rust_ident: field.rust_ident.clone(),
                gql_field: field.gql_field.clone(),
                column: column.clone(),
                target_type: target_type.clone(),
            }),
            // Nullable scalar/FK, or a junction (`*Connection`) field: not
            // part of the NOT NULL ref set.
            FieldRole::ScalarColumn { .. }
            | FieldRole::ForeignKey { .. }
            | FieldRole::Junction { .. } => {}
        }
    }

    RefFragment {
        type_name: fragment.graphql_type.clone(),
        scalar_members,
        fk_members,
    }
}

/// The Rust field type for a scalar ref member.
fn scalar_field_type(rust_base_type: &str, list: bool) -> TokenStream {
    let base = if rust_base_type == "Id" {
        quote! { cynic::Id }
    } else {
        let ident = format_ident!("{}", rust_base_type);
        quote! { #ident }
    };
    if list {
        quote! { Vec<#base> }
    } else {
        base
    }
}

/// Emit `<T>Ref` as a `cynic::QueryFragment` struct: the scalar members
/// directly, and for each FK member a nested `<Target>IdRef` struct (deduped
/// by target type) that selects only the target's `id` -- the degraded
/// selection that keeps generation from ever recursing into the target's own
/// `*Ref`.
pub fn emit_ref_fragment(ref_fragment: &RefFragment) -> TokenStream {
    let ref_ident = format_ident!("{}Ref", ref_fragment.type_name);
    let graphql_type = &ref_fragment.type_name;

    let scalar_fields = ref_fragment.scalar_members.iter().map(|member| {
        let ident = format_ident!("{}", member.rust_ident);
        let ty = scalar_field_type(&member.rust_base_type, member.list);
        quote! { pub #ident: #ty, }
    });

    let mut seen_targets = BTreeSet::new();
    let id_ref_structs = ref_fragment
        .fk_members
        .iter()
        .filter(|member| seen_targets.insert(member.target_type.clone()))
        .map(|member| {
            let id_ref_ident = format_ident!("{}IdRef", member.target_type);
            let target_type = &member.target_type;
            quote! {
                #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
                #[cynic(graphql_type = #target_type)]
                pub struct #id_ref_ident {
                    pub id: cynic::Id,
                }
            }
        });

    let fk_fields = ref_fragment.fk_members.iter().map(|member| {
        let ident = format_ident!("{}", member.rust_ident);
        let id_ref_ident = format_ident!("{}IdRef", member.target_type);
        quote! { pub #ident: #id_ref_ident, }
    });

    quote! {
        #( #id_ref_structs )*

        #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
        #[cynic(graphql_type = #graphql_type)]
        pub struct #ref_ident {
            #( #scalar_fields )*
            #( #fk_fields )*
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{emit_ref_fragment, ref_fragment};
    use crate::schema_model::Schema;
    use crate::selection_model::parse_fragments;
    use crate::test_fixtures::{ISSUE_FRAGMENT_SRC, ISSUE_SDL, issue_generated_types};

    fn issue_ref() -> super::RefFragment {
        let schema = Schema::parse(ISSUE_SDL).expect("SDL parses");
        let fragments = parse_fragments(ISSUE_FRAGMENT_SRC).expect("fragment source parses");
        let issue = fragments
            .iter()
            .find(|f| f.rust_name == "Issue")
            .expect("Issue fragment present");
        let generated_types = issue_generated_types();

        ref_fragment(issue, &schema, &generated_types)
    }

    #[test]
    fn issue_ref_has_exactly_the_expected_scalar_members() {
        let issue_ref = issue_ref();
        let scalar_idents: BTreeSet<&str> = issue_ref
            .scalar_members
            .iter()
            .map(|m| m.rust_ident.as_str())
            .collect();

        assert_eq!(
            scalar_idents,
            BTreeSet::from([
                "id",
                "identifier",
                "title",
                "priority_label",
                "priority",
                "created_at",
                "updated_at",
            ])
        );
    }

    #[test]
    fn issue_ref_has_exactly_the_expected_fk_members() {
        let issue_ref = issue_ref();
        let fks: BTreeSet<(&str, &str)> = issue_ref
            .fk_members
            .iter()
            .map(|m| (m.rust_ident.as_str(), m.target_type.as_str()))
            .collect();

        assert_eq!(
            fks,
            BTreeSet::from([("state", "WorkflowState"), ("team", "Team")])
        );
    }

    #[test]
    fn issue_ref_excludes_junctions_and_nullable_fields() {
        let issue_ref = issue_ref();
        let all_idents: BTreeSet<&str> = issue_ref
            .scalar_members
            .iter()
            .map(|m| m.rust_ident.as_str())
            .chain(issue_ref.fk_members.iter().map(|m| m.rust_ident.as_str()))
            .collect();

        for excluded in [
            "labels",
            "parent",
            "assignee",
            "description",
            "project",
            "cycle",
            "creator",
        ] {
            assert!(
                !all_idents.contains(excluded),
                "expected `{excluded}` to be absent from IssueRef"
            );
        }
    }

    // Mutually-recursive fixture: `A.b: B` and `B.a: A`, both non-`Option`.
    // Generating both refs completing at all -- there is no recursion to
    // diverge -- proves the termination invariant.
    const MUTUAL_RECURSION_SDL: &str = r"
        interface Node {
            id: ID!
        }

        type A implements Node {
            id: ID!
            b: B!
        }

        type B implements Node {
            id: ID!
            a: A!
        }
    ";

    const MUTUAL_RECURSION_FRAGMENT_SRC: &str = r#"
        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "A")]
        pub struct A {
            pub id: cynic::Id,
            pub b: B,
        }

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "B")]
        pub struct B {
            pub id: cynic::Id,
            pub a: A,
        }
    "#;

    #[test]
    fn ref_fragment_generation_terminates_for_mutually_recursive_types() {
        let schema = Schema::parse(MUTUAL_RECURSION_SDL).expect("SDL parses");
        let fragments =
            parse_fragments(MUTUAL_RECURSION_FRAGMENT_SRC).expect("fragment source parses");
        let a = fragments
            .iter()
            .find(|f| f.rust_name == "A")
            .expect("A fragment present");
        let b = fragments
            .iter()
            .find(|f| f.rust_name == "B")
            .expect("B fragment present");
        let generated_types = BTreeSet::from(["A", "B"]);

        let a_ref = ref_fragment(a, &schema, &generated_types);
        let b_ref = ref_fragment(b, &schema, &generated_types);

        assert_eq!(
            a_ref
                .fk_members
                .iter()
                .map(|m| m.rust_ident.as_str())
                .collect::<Vec<_>>(),
            vec!["b"]
        );
        assert_eq!(
            b_ref
                .fk_members
                .iter()
                .map(|m| m.rust_ident.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
    }

    #[test]
    fn ref_fragment_generation_terminates_for_the_real_issue_self_reference() {
        // `Issue.parent` is `Option<Parent>` (schema type `Issue`), so this
        // exercises the same code path against an actual self-reference.
        issue_ref();
    }

    #[test]
    fn issue_ref_token_stream_snapshot() {
        let tokens = emit_ref_fragment(&issue_ref());
        let pretty = crate::format_generated("", tokens);
        insta::assert_snapshot!(pretty);
    }
}
