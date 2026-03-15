use std::sync::Arc;

use anyhow::{bail, Result};
use orchestrator_core::ServiceHub;
use orchestrator_web_api::{WebApiContext, WebApiService};
use orchestrator_web_server::{WebServer, WebServerConfig};
use serde_json::json;

use crate::{print_ok, print_value, WebCommand};

pub(crate) async fn handle_web(
    command: WebCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        WebCommand::Serve(args) => {
            let (default_page_size, max_page_size) =
                validate_page_size_config(args.page_size_default, args.page_size_max)?;
            let url = build_url(&args.host, args.port, "/");
            if args.open {
                open_in_browser(&url)?;
            }

            let api_context = Arc::new(WebApiContext {
                hub,
                project_root: project_root.to_string(),
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            });
            let api = WebApiService::new(api_context);
            let server = WebServer::new(
                WebServerConfig {
                    host: args.host.clone(),
                    port: args.port,
                    assets_dir: args.assets_dir.clone(),
                    api_only: args.api_only,
                    default_page_size,
                    max_page_size,
                },
                api,
            );

            print_value(
                json!({
                    "message": "web server starting",
                    "url": url,
                    "host": args.host,
                    "port": args.port,
                    "api_only": args.api_only,
                    "assets_dir": args.assets_dir,
                    "page_size_default": default_page_size,
                    "page_size_max": max_page_size,
                }),
                json,
            )?;

            server.run().await
        }
        WebCommand::Open(args) => {
            let path = normalize_web_path(&args.path);
            let url = build_url(&args.host, args.port, &path);
            open_in_browser(&url)?;
            if json {
                print_value(
                    json!({
                        "message": "browser opened",
                        "url": url,
                    }),
                    true,
                )
            } else {
                print_ok(&format!("opened {url}"), false);
                Ok(())
            }
        }
    }
}

fn build_url(host: &str, port: u16, path: &str) -> String {
    format!("http://{host}:{port}{path}")
}

fn normalize_web_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return "/".to_string();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn open_in_browser(url: &str) -> Result<()> {
    webbrowser::open(url).map(|_| ()).map_err(|error| anyhow::anyhow!("failed to open browser: {error}"))
}

fn validate_page_size_config(default_page_size: usize, max_page_size: usize) -> Result<(usize, usize)> {
    if default_page_size == 0 {
        bail!("page-size-default must be at least 1");
    }
    if max_page_size == 0 {
        bail!("page-size-max must be at least 1");
    }
    if default_page_size > max_page_size {
        bail!("page-size-default cannot exceed page-size-max");
    }
    Ok((default_page_size, max_page_size))
}

#[cfg(test)]
mod tests {
    use super::validate_page_size_config;

    #[test]
    fn validates_page_size_config_successfully() {
        let validated = validate_page_size_config(50, 200).expect("page sizes should validate");
        assert_eq!(validated, (50, 200));
    }

    #[test]
    fn rejects_zero_default_page_size() {
        let error = validate_page_size_config(0, 200).expect_err("zero default page size should fail");
        assert!(
            error.to_string().contains("page-size-default must be at least 1"),
            "error should describe invalid default page size"
        );
    }

    #[test]
    fn rejects_zero_max_page_size() {
        let error = validate_page_size_config(50, 0).expect_err("zero max page size should fail");
        assert!(
            error.to_string().contains("page-size-max must be at least 1"),
            "error should describe invalid max page size"
        );
    }

    #[test]
    fn rejects_default_page_size_greater_than_max() {
        let error = validate_page_size_config(201, 200).expect_err("default page size greater than max should fail");
        assert!(
            error.to_string().contains("page-size-default cannot exceed page-size-max"),
            "error should describe default greater than max"
        );
    }
}
