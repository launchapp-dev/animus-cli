pub mod cleanup;
pub mod config;
pub mod ipc;
pub mod lock;
pub mod output;
pub mod providers;
pub mod recording;
pub mod runner;
pub mod sandbox;
pub mod telemetry;

#[cfg(test)]
pub fn test_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}
