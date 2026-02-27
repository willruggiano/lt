// build_sort() -- generated from [[sort_field]] entries in
// build/search_filter_fields.toml by build.rs (bd-2w5).
//
// The generated function maps each SortField variant to its IssueSortInput
// field key and wraps it in the Linear GraphQL sort JSON envelope.
use super::SortField;

include!(concat!(env!("OUT_DIR"), "/sort_build.rs"));
