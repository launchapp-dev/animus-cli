use std::sync::Arc;

use anyhow::Result;
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
    webbrowser::open(url)
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("failed to open browser: {error}"))
}
