//! The `PageInfo` fragment shared by every paginated query (notifications,
//! issues, comments): one cynic fragment reused across operations rather than
//! copy-pasted per module.

use crate::schema;

#[derive(cynic::QueryFragment)]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}
