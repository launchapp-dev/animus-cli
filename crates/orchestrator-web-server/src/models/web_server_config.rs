#[derive(Debug, Clone)]
pub struct WebServerConfig {
    pub host: String,
    pub port: u16,
    pub assets_dir: Option<String>,
    pub api_only: bool,
}

impl Default for WebServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 4173,
            assets_dir: None,
            api_only: false,
        }
    }
}
