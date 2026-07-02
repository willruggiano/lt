use anyhow::{Result, anyhow};

use crate::client::HttpTransport;

/// Load the stored token and fetch the viewer identity (the `lt auth status`
/// data path). Presentation (printing `user:`/`id:`/`organization:`) lives in
/// the CLI layer.
pub fn viewer_from_config() -> Result<lt_types::viewer::User> {
    let token = lt_config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;

    let transport = HttpTransport::new(token.access_token);
    crate::viewer::fetch(&transport)
}
