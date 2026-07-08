use anyhow::{Result, anyhow};

use crate::query::viewer::ViewerQuery;
use crate::transport::{HttpTransport, execute};

/// Load the stored token and fetch the viewer identity (the `lt auth status`
/// data path). Presentation (printing `user:`/`id:`/`organization:`) lives in
/// the CLI layer.
pub fn viewer_from_config() -> Result<crate::query::viewer::Viewer> {
    let token = lt_config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;

    let transport = HttpTransport::new(token.access_token);
    // `Query.viewer` is non-null on the wire; `ViewerQuery::Output` is
    // `Option` only for the local cache read's missing-row case.
    execute::<ViewerQuery>(&transport, ())?
        .ok_or_else(|| anyhow!("viewer query returned no viewer"))
}
