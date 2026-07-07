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

    /// The schema-level kind of the named type, if it is defined.
    ///
    /// GraphQL's five built-in scalars (`String`, `Int`, `Float`, `Boolean`,
    /// `ID`) are never `scalar`-declared in SDL -- confirmed against
    /// `build/linear-schema-definition.graphql`, which declares only its
    /// custom scalars (`DateTime`, `JSON`, `UUID`, ...) -- so they are
    /// recognised without a document lookup.
    pub fn type_kind(&self, name: &str) -> Option<TypeKind> {
        if is_builtin_scalar(name) {
            return Some(TypeKind::Scalar);
        }
        self.0.definitions().find_map(|def| match def.as_type()? {
            TypeDefinition::Scalar(scalar) if scalar.name() == name => Some(TypeKind::Scalar),
            TypeDefinition::Enum(enum_def) if enum_def.name() == name => Some(TypeKind::Enum),
            TypeDefinition::Object(obj) if obj.name() == name => Some(TypeKind::Object {
                implements_node: obj.implements_interfaces().any(|iface| iface == "Node"),
            }),
            _ => None,
        })
    }
}

fn is_builtin_scalar(name: &str) -> bool {
    matches!(name, "String" | "Int" | "Float" | "Boolean" | "ID")
}

/// The schema-level kind of a named type, as needed to classify a fragment
/// field's storage role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Scalar,
    Enum,
    Object { implements_node: bool },
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
    use super::{Schema, TypeKind};

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

        scalar DateTime

        enum Priority {
            LOW
            HIGH
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
    fn type_kind_identifies_scalar_enum_and_object() {
        let schema = Schema::parse(SDL).expect("SDL parses");
        assert_eq!(schema.type_kind("DateTime"), Some(TypeKind::Scalar));
        assert_eq!(schema.type_kind("Priority"), Some(TypeKind::Enum));
        assert_eq!(schema.type_kind("String"), Some(TypeKind::Scalar));
        assert_eq!(schema.type_kind("ID"), Some(TypeKind::Scalar));
        assert_eq!(
            schema.type_kind("Issue"),
            Some(TypeKind::Object {
                implements_node: true
            })
        );
        assert_eq!(
            schema.type_kind("Comment"),
            Some(TypeKind::Object {
                implements_node: false
            })
        );
        assert_eq!(schema.type_kind("Nonexistent"), None);
    }

    #[test]
    fn object_returns_none_for_unknown_name() {
        let schema = Schema::parse(SDL).expect("SDL parses");
        assert!(schema.object("Nonexistent").is_none());
    }
}
