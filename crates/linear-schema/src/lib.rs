//! Owns the registered Linear cynic schema markers, isolating cynic's schema
//! codegen behind a single crate.

#[cynic::schema("linear")]
pub mod linear {}

#[cfg(test)]
mod tests {
    #[derive(cynic::QueryFragment)]
    #[cynic(schema_module = "crate::linear", graphql_type = "WorkflowState")]
    struct WorkflowState {
        id: cynic::Id,
        name: String,
    }

    #[test]
    fn marker_module_resolves() {
        let state = WorkflowState {
            id: cynic::Id::new("1"),
            name: "Todo".to_string(),
        };
        assert_eq!(state.id.inner(), "1");
        assert_eq!(state.name, "Todo");
    }
}
