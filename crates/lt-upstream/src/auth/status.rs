use std::io::Write;

use anyhow::{Result, anyhow};
use lt_types::viewer::ViewerQuery;
use serde_json::Value;

use crate::client::{HttpTransport, query_as};

pub fn run(out: &mut dyn Write) -> Result<()> {
    let token = lt_config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;

    let transport = HttpTransport::new(token.access_token);
    let data: ViewerQuery = query_as(&transport, &lt_types::viewer::query(), Value::Null)?;
    let viewer = data.viewer;

    writeln!(out, "user:         {}", viewer.name)?;
    writeln!(out, "id:           {}", viewer.id.inner())?;
    writeln!(
        out,
        "organization: {} ({})",
        viewer.organization.name, viewer.organization.url_key
    )?;

    Ok(())
}
