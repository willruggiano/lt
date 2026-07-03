use std::io::Write;

use anyhow::{Context, Result, anyhow};

const BOOTSTRAP_URL: &str = "https://client-api.linear.app/sync/bootstrap";

/// Print up to the first five lines of a response body, truncating long lines.
fn print_body_preview(out: &mut dyn Write, body: &str) -> Result<()> {
    let lines: Vec<&str> = body.lines().take(5).collect();
    if lines.is_empty() {
        writeln!(out, "(empty body)")?;
        return Ok(());
    }
    writeln!(out, "--- first {} line(s) of body ---", lines.len())?;
    for line in &lines {
        // Truncate very long lines so the terminal stays readable.
        if line.len() > 200 {
            writeln!(out, "{}...", &line[..200])?;
        } else {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

pub fn run(out: &mut dyn Write, override_token: Option<String>) -> Result<()> {
    let (raw_token, label) = if let Some(t) = override_token {
        (t, "cli --token flag")
    } else {
        let stored = lt_config::load_token()?
            .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;
        (stored.access_token, "stored OAuth token")
    };

    writeln!(out, "endpoint:   {BOOTSTRAP_URL}")?;
    writeln!(out, "params:     type=full&onlyModels=Issue")?;
    writeln!(out, "auth:       Bearer <token> (source: {label})")?;
    writeln!(out)?;

    // Linear personal API keys must be sent raw (no "Bearer" prefix).
    // OAuth tokens require "Bearer <token>".
    let auth_header = if raw_token.starts_with("lin_api_") {
        raw_token.clone()
    } else {
        format!("Bearer {raw_token}")
    };

    let result = ureq::get(BOOTSTRAP_URL)
        .query("type", "full")
        .query("onlyModels", "Issue")
        .header("Authorization", &auth_header)
        .config()
        .http_status_as_error(false)
        .build()
        .call();

    match result {
        Ok(mut res) => {
            let status = res.status();
            let content_type = res
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("(none)")
                .to_string();

            if status.is_success() {
                writeln!(out, "status:       {status}")?;
                writeln!(out, "content-type: {content_type}")?;
                writeln!(out)?;

                let body = res
                    .body_mut()
                    .read_to_string()
                    .context("reading response body")?;
                print_body_preview(out, &body)?;
            } else {
                writeln!(out, "status:       {} (error)", status.as_u16())?;
                writeln!(out, "content-type: {content_type}")?;
                writeln!(out)?;

                let body = res
                    .body_mut()
                    .read_to_string()
                    .context("reading error response body")?;
                writeln!(out, "--- error body (up to 500 chars) ---")?;
                writeln!(out, "{}", &body[..body.len().min(500)])?;
            }
        }
        Err(e) => {
            return Err(anyhow::Error::from(e).context("connecting to sync API"));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(body: &str) -> String {
        let mut buf = Vec::new();
        print_body_preview(&mut buf, body).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn empty_body_is_reported() {
        assert_eq!(preview(""), "(empty body)\n");
    }

    #[test]
    fn shows_up_to_five_lines() {
        let out = preview("a\nb\nc\nd\ne\nf\ng");
        assert!(out.contains("first 5 line(s)"));
        assert!(out.contains("\na\n") && out.contains("\ne\n"));
        // Lines past the fifth are dropped.
        assert!(!out.contains("\nf\n"));
    }

    #[test]
    fn truncates_long_lines_at_200_chars() {
        let long = "x".repeat(250);
        let out = preview(&long);
        assert!(out.contains(&format!("{}...", "x".repeat(200))));
        assert!(!out.contains(&"x".repeat(201)));
    }
}
