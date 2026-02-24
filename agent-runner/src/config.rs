use std::path::PathBuf;

pub fn app_config_dir() -> PathBuf {
    protocol::Config::global_config_dir()
}
