//! Thin typed wrapper over `cynic_parser` (offline SDL parse) for the
//! schema-driven classification that T3+ builds on top of.

use cynic_parser::type_system::{ObjectDefinition, TypeDefinition};
use cynic_parser::{Error, TypeSystemDocument};

/// A parsed GraphQL SDL document.
pub struct Schema(TypeSystemDocument);

impl Schema {
    /// Parse an SDL string into a document.
    pub fn parse(sdl: &str) -> Result<Self, Error> {
        Ok(Self(cynic_parser::parse_type_system_document(sdl)?))
    }

    /// Look up an object definition by name.
    pub fn object(&self, name: &str) -> Option<Object<'_>> {
        self.0.definitions().find_map(|def| match def.as_type()? {
            TypeDefinition::Object(obj) if obj.name() == name => Some(Object(obj)),
            _ => None,
        })
    }
}

/// A single GraphQL object type definition.
pub struct Object<'a>(ObjectDefinition<'a>);

impl<'a> Object<'a> {
    /// Each field's name paired with its base type name (NonNull/List
    /// wrappers already stripped by [`cynic_parser::type_system::Type::name`]).
    pub fn fields(&self) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        self.0
            .fields()
            .map(|field| (field.name(), field.ty().name()))
    }

    /// Whether this object implements the `Node` interface.
    pub fn implements_node(&self) -> bool {
        self.0.implements_interfaces().any(|iface| iface == "Node")
    }
}

#[cfg(test)]
mod tests {
    use super::Schema;

    const SDL: &str = r"
        interface Node {
            id: ID!
        }

        type Issue implements Node {
            id: ID!
            title: String!
            parent: Issue
            labels: [String!]!
        }

        type Comment {
            id: ID!
        }
    ";

    #[test]
    fn object_fields_yield_name_and_base_type() {
        let schema = Schema::parse(SDL).expect("SDL parses");
        let issue = schema.object("Issue").expect("Issue object present");

        let fields: Vec<(&str, &str)> = issue.fields().collect();
        assert_eq!(
            fields,
            vec![
                ("id", "ID"),
                ("title", "String"),
                ("parent", "Issue"),
                ("labels", "String"),
            ]
        );
    }

    #[test]
    fn implements_node_is_true_only_for_the_node_object() {
        let schema = Schema::parse(SDL).expect("SDL parses");
        assert!(
            schema
                .object("Issue")
                .expect("Issue object present")
                .implements_node()
        );
        assert!(
            !schema
                .object("Comment")
                .expect("Comment object present")
                .implements_node()
        );
    }

    #[test]
    fn object_returns_none_for_unknown_name() {
        let schema = Schema::parse(SDL).expect("SDL parses");
        assert!(schema.object("Nonexistent").is_none());
    }
}
