use std::ffi::{OsStr, OsString};

pub struct EnvVarGuard {
    key: String,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    pub fn set(key: &str, value: Option<&str>) -> Self {
        Self::set_os(key, value.map(OsStr::new))
    }

    pub fn set_os(key: &str, value: Option<&OsStr>) -> Self {
        let previous = std::env::var_os(key);
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        Self { key: key.to_string(), previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(&self.key, previous);
        } else {
            std::env::remove_var(&self.key);
        }
    }
}
