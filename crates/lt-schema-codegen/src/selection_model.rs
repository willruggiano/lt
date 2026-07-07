//! `syn`-based extractor over cynic `QueryFragment` struct source.
//!
//! Produces, per fragment field, the facts that schema-driven classification
//! needs: the GraphQL field it selects, its base named type, and whether it
//! is nullable and/or a list.

use syn::punctuated::Punctuated;
use syn::{Attribute, Field, Fields, GenericArgument, Item, ItemStruct, PathArguments, Type};

/// A struct deriving `cynic::QueryFragment`: its GraphQL type and fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    pub rust_name: String,
    pub graphql_type: String,
    pub fields: Vec<FragmentField>,
}

/// One named field of a [`Fragment`].
#[derive(Debug, Clone, PartialEq)]
pub struct FragmentField {
    pub rust_ident: String,
    pub gql_field: String,
    pub base_type: String,
    pub nullable: bool,
    pub list: bool,
}

/// Parse fragment source text and extract every struct deriving
/// `cynic::QueryFragment`. Other items (e.g. a plain response envelope
/// struct) are ignored.
pub fn parse_fragments(query_src: &str) -> syn::Result<Vec<Fragment>> {
    syn::parse_file(query_src)?
        .items
        .into_iter()
        .filter_map(|item| match item {
            Item::Struct(item_struct) if derives_query_fragment(&item_struct) => Some(item_struct),
            _ => None,
        })
        .map(|item_struct| parse_fragment(&item_struct))
        .collect()
}

fn derives_query_fragment(item_struct: &ItemStruct) -> bool {
    item_struct.attrs.iter().any(|attr| {
        attr.path().is_ident("derive")
            && attr
                .parse_args_with(Punctuated::<syn::Path, syn::Token![,]>::parse_terminated)
                .is_ok_and(|paths| {
                    paths.iter().any(|path| {
                        path.segments
                            .last()
                            .is_some_and(|seg| seg.ident == "QueryFragment")
                    })
                })
    })
}

fn parse_fragment(item_struct: &ItemStruct) -> syn::Result<Fragment> {
    let rust_name = item_struct.ident.to_string();
    let graphql_type =
        attr_str_value(&item_struct.attrs, "graphql_type")?.unwrap_or_else(|| rust_name.clone());

    let Fields::Named(named) = &item_struct.fields else {
        return Err(syn::Error::new(
            item_struct.ident.span(),
            "cynic::QueryFragment struct must have named fields",
        ));
    };

    let fields = named
        .named
        .iter()
        .map(parse_field)
        .collect::<syn::Result<_>>()?;

    Ok(Fragment {
        rust_name,
        graphql_type,
        fields,
    })
}

fn parse_field(field: &Field) -> syn::Result<FragmentField> {
    let rust_ident = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new(proc_macro2::Span::call_site(), "expected a named field"))?
        .to_string();

    let gql_field =
        attr_str_value(&field.attrs, "rename")?.unwrap_or_else(|| to_camel_case(&rust_ident));

    let (nullable, ty) = strip_wrapper(&field.ty, "Option");
    let (list, ty) = strip_wrapper(ty, "Vec");
    let base_type = base_type_name(ty)?;

    Ok(FragmentField {
        rust_ident,
        gql_field,
        base_type,
        nullable,
        list,
    })
}

/// The string value of `#[cynic(<key> = "...")]`, if present among `attrs`.
fn attr_str_value(attrs: &[Attribute], key: &str) -> syn::Result<Option<String>> {
    let mut found = None;
    for attr in attrs {
        if !attr.path().is_ident("cynic") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(key) {
                let lit: syn::LitStr = meta.value()?.parse()?;
                found = Some(lit.value());
            }
            Ok(())
        })?;
    }
    Ok(found)
}

/// Strip a leading `wrapper<_>` (e.g. `Option` or `Vec`) from `ty`, returning
/// whether it was present and the inner type (or `ty` unchanged if absent).
fn strip_wrapper<'a>(ty: &'a Type, wrapper: &str) -> (bool, &'a Type) {
    if let Type::Path(type_path) = ty
        && let Some(seg) = type_path.path.segments.last()
        && seg.ident == wrapper
        && let PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(GenericArgument::Type(inner)) = args.args.first()
    {
        return (true, inner);
    }
    (false, ty)
}

/// The final path segment ident of a named type, e.g. `Id` for `cynic::Id`.
fn base_type_name(ty: &Type) -> syn::Result<String> {
    match ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|seg| seg.ident.to_string())
            .ok_or_else(|| syn::Error::new(proc_macro2::Span::call_site(), "empty type path")),
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "unsupported field type shape",
        )),
    }
}

/// Convert `snake_case` to `camelCase` -- cynic's default GraphQL field
/// rename (`cynic-codegen/src/fragment_derive/mod.rs` -> `idents/old_ident.rs`).
pub fn to_camel_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, word) in s.split('_').enumerate() {
        if i == 0 {
            out.push_str(word);
        } else {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{parse_fragments, to_camel_case};

    // Mirrors `lt-types/src/types.rs:80-99` (the `Issue` fragment), plus a
    // rename-covering struct and a non-`QueryFragment` struct to prove it is
    // ignored.
    const FIXTURE: &str = r#"
        #[derive(Deserialize)]
        pub struct GraphqlResponse<T> {
            pub data: Option<T>,
            pub errors: Option<Vec<GraphqlError>>,
        }

        #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
        #[cynic(graphql_type = "Issue")]
        pub struct Issue {
            pub id: cynic::Id,
            pub identifier: String,
            pub title: String,
            pub priority_label: String,
            pub priority: Priority,
            pub state: WorkflowState,
            pub assignee: Option<User>,
            pub team: Team,
            pub description: Option<String>,
            pub labels: IssueLabelConnection,
            pub project: Option<Project>,
            pub cycle: Option<Cycle>,
            pub creator: Option<User>,
            pub parent: Option<Parent>,
            pub created_at: DateTime,
            pub updated_at: DateTime,
        }

        #[derive(cynic::QueryFragment, Debug)]
        struct Custom {
            #[cynic(rename = "customName")]
            pub custom_field: Option<Vec<Label>>,
        }
    "#;

    fn field<'a>(
        fragments: &'a [super::Fragment],
        struct_name: &str,
        field_name: &str,
    ) -> &'a super::FragmentField {
        fragments
            .iter()
            .find(|f| f.rust_name == struct_name)
            .unwrap_or_else(|| panic!("missing fragment {struct_name}"))
            .fields
            .iter()
            .find(|f| f.rust_ident == field_name)
            .unwrap_or_else(|| panic!("missing field {field_name} on {struct_name}"))
    }

    #[test]
    fn parse_fragments_extracts_issue_fragment_fields() {
        let fragments = parse_fragments(FIXTURE).expect("fixture parses");
        let issue = fragments
            .iter()
            .find(|f| f.rust_name == "Issue")
            .expect("Issue fragment present");
        assert_eq!(issue.graphql_type, "Issue");

        let priority_label = field(&fragments, "Issue", "priority_label");
        assert_eq!(priority_label.gql_field, "priorityLabel");
        assert_eq!(priority_label.base_type, "String");
        assert!(!priority_label.nullable);
        assert!(!priority_label.list);

        assert_eq!(
            field(&fragments, "Issue", "created_at").gql_field,
            "createdAt"
        );
        assert_eq!(
            field(&fragments, "Issue", "updated_at").gql_field,
            "updatedAt"
        );

        let parent = field(&fragments, "Issue", "parent");
        assert!(parent.nullable);
        assert_eq!(parent.base_type, "Parent");

        let assignee = field(&fragments, "Issue", "assignee");
        assert!(assignee.nullable);
        assert_eq!(assignee.base_type, "User");

        let labels = field(&fragments, "Issue", "labels");
        assert_eq!(labels.base_type, "IssueLabelConnection");
        assert!(!labels.nullable);

        assert_eq!(field(&fragments, "Issue", "id").base_type, "Id");
    }

    #[test]
    fn parse_fragments_prefers_rename_over_camel_case() {
        let fragments = parse_fragments(FIXTURE).expect("fixture parses");
        let custom = field(&fragments, "Custom", "custom_field");
        assert_eq!(custom.gql_field, "customName");
        assert!(custom.nullable);
        assert!(custom.list);
        assert_eq!(custom.base_type, "Label");
    }

    #[test]
    fn parse_fragments_ignores_non_query_fragment_structs() {
        let fragments = parse_fragments(FIXTURE).expect("fixture parses");
        assert!(fragments.iter().all(|f| f.rust_name != "GraphqlResponse"));
    }

    #[test]
    fn parse_fragments_rejects_unsupported_field_type_shape() {
        const BAD: &str = r"
            #[derive(cynic::QueryFragment)]
            struct Bad {
                pub value: &'static str,
            }
        ";
        assert!(parse_fragments(BAD).is_err());
    }

    #[test]
    fn to_camel_case_converts_snake_case() {
        assert_eq!(to_camel_case("priority_label"), "priorityLabel");
        assert_eq!(to_camel_case("created_at"), "createdAt");
        assert_eq!(to_camel_case("title"), "title");
    }
}
