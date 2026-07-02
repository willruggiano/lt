//! The `PageInfo` fragment shared by every paginated query (notifications,
//! issues, comments): one cynic fragment reused across operations rather than
//! copy-pasted per module.

use crate::schema;

#[derive(Debug, cynic::QueryFragment)]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

/// One page of a connection: the nodes plus the cursor state needed to fetch
/// the next page.
#[derive(Debug)]
pub struct Page<T> {
    pub nodes: Vec<T>,
    pub info: PageInfo,
}
