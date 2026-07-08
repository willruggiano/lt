//! Search grammar emission from trimmed, scalar-only `IssueFilter`/
//! `IssueSortInput` mirrors.
//!
//! Scoped to scalars only: every relation/FK field (assignee, state, team,
//! project, cycle, creator, labels) is out of scope and absent from the
//! mirrors this module reads. Sorting a scalar field needs no join, so it is
//! identity: each [`SortField`] variant maps directly to `ORDER BY` its
//! generated column.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::classify::to_snake_case;
use crate::selection_model::Fragment;
use crate::to_pascal_case;

/// Emit the `SortField` enum and its identity `order_by()` mapping, one
/// variant/arm per scalar field of a trimmed `IssueSortInput` mirror.
pub fn emit_sort_grammar(sort_input: &Fragment) -> TokenStream {
    let variants: Vec<_> = sort_input
        .fields
        .iter()
        .map(|f| format_ident!("{}", to_pascal_case(&f.rust_ident)))
        .collect();

    let order_by_arms = sort_input.fields.iter().map(|f| {
        let variant = format_ident!("{}", to_pascal_case(&f.rust_ident));
        let order_by = format!("ORDER BY {}", to_snake_case(&f.gql_field));
        quote! { SortField::#variant => #order_by, }
    });

    quote! {
        /// A sort field over `IssueSortInput`'s scalar fields.
        #[derive(Debug, Clone, PartialEq)]
        pub enum SortField {
            #( #variants, )*
        }

        impl SortField {
            /// The identity `ORDER BY` clause for this field.
            pub fn order_by(&self) -> &'static str {
                match self {
                    #( #order_by_arms )*
                }
            }
        }
    }
}

/// Emit the `StemKey`/`StemKind` enums, one variant per scalar field of a
/// trimmed `IssueFilter` mirror.
pub fn emit_filter_grammar(filter: &Fragment) -> TokenStream {
    let variants: Vec<_> = filter
        .fields
        .iter()
        .map(|f| format_ident!("{}", to_pascal_case(&f.rust_ident)))
        .collect();

    quote! {
        /// The key side of a filter stem token.
        #[derive(Debug, Clone, PartialEq)]
        pub enum StemKey {
            #( #variants, )*
        }

        /// The fully-parsed meaning of a recognised filter stem.
        #[derive(Debug, Clone, PartialEq)]
        pub enum StemKind {
            #( #variants { value: String }, )*
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{emit_filter_grammar, emit_sort_grammar};
    use crate::selection_model::{Fragment, parse_input_objects};

    // Trimmed to the scalar subset only: createdAt, updatedAt, priority,
    // title. Every relation field (assignee, state, team, project, cycle,
    // creator, labels) is out of scope and dropped.
    const SORT_INPUT_FIXTURE: &str = r#"
        #[derive(cynic::InputObject, Debug)]
        #[cynic(graphql_type = "IssueSortInput")]
        pub struct IssueSortInput {
            pub created_at: Option<DateTime>,
            pub updated_at: Option<DateTime>,
            pub priority: Option<Priority>,
            pub title: Option<String>,
        }
    "#;

    const FILTER_FIXTURE: &str = r#"
        #[derive(cynic::InputObject, Debug)]
        #[cynic(graphql_type = "IssueFilter")]
        pub struct IssueFilter {
            pub created_at: Option<DateTime>,
            pub updated_at: Option<DateTime>,
            pub priority: Option<Priority>,
            pub title: Option<String>,
        }
    "#;

    fn parse_one(src: &str, rust_name: &str) -> Fragment {
        parse_input_objects(src)
            .expect("fixture parses")
            .into_iter()
            .find(|f| f.rust_name == rust_name)
            .unwrap_or_else(|| panic!("missing {rust_name}"))
    }

    #[test]
    fn sort_grammar_snapshot() {
        let sort_input = parse_one(SORT_INPUT_FIXTURE, "IssueSortInput");
        let tokens = emit_sort_grammar(&sort_input);
        insta::assert_snapshot!(crate::format_generated("", tokens));
    }

    #[test]
    fn filter_grammar_snapshot() {
        let filter = parse_one(FILTER_FIXTURE, "IssueFilter");
        let tokens = emit_filter_grammar(&filter);
        insta::assert_snapshot!(crate::format_generated("", tokens));
    }
}
