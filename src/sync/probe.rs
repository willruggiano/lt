use anyhow::{Context, Result, anyhow};

use crate::config;

const BOOTSTRAP_URL: &str = "https://client-api.linear.app/sync/bootstrap";

pub fn run(override_token: Option<String>) -> Result<()> {
    let (raw_token, label) = match override_token {
        Some(t) => (t, "cli --token flag"),
        None => {
            let stored = config::load_token()?
                .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;
            (stored.access_token, "stored OAuth token")
        }
    };

    println!("endpoint:   {}", BOOTSTRAP_URL);
    println!("params:     type=full&onlyModels=Issue");
    println!("auth:       Bearer <token> (source: {})", label);
    println!();

    // Linear personal API keys must be sent raw (no "Bearer" prefix).
    // OAuth tokens require "Bearer <token>".
    let auth_header = if raw_token.starts_with("lin_api_") {
        raw_token.clone()
    } else {
        format!("Bearer {}", raw_token)
    };

    let result = ureq::get(BOOTSTRAP_URL)
        .query("type", "full")
        .query("onlyModels", "Issue")
        .set("Authorization", &auth_header)
        .call();

    match result {
        Ok(res) => {
            let status = res.status();
            let content_type = res.header("content-type").unwrap_or("(none)").to_string();
            println!("status:       {}", status);
            println!("content-type: {}", content_type);
            println!();

            let body = res.into_string().context("reading response body")?;
            let lines: Vec<&str> = body.lines().take(5).collect();
            if lines.is_empty() {
                println!("(empty body)");
            } else {
                println!("--- first {} line(s) of body ---", lines.len());
                for line in &lines {
                    // Truncate very long lines so the terminal stays readable
                    if line.len() > 200 {
                        println!("{}...", &line[..200]);
                    } else {
                        println!("{}", line);
                    }
                }
            }
        }
        Err(ureq::Error::Status(code, res)) => {
            let content_type = res.header("content-type").unwrap_or("(none)").to_string();
            println!("status:       {} (error)", code);
            println!("content-type: {}", content_type);
            println!();

            let body = res.into_string().unwrap_or_default();
            println!("--- error body (up to 500 chars) ---");
            println!("{}", &body[..body.len().min(500)]);
        }
        Err(e) => {
            return Err(anyhow::Error::from(e).context("connecting to sync API"));
        }
    }

    Ok(())
}
