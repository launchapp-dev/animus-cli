use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static ENV_LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub struct EnvVarGuard {
    key: String,
    previous: Option<OsString>,
    _lock: Option<MutexGuard<'static, ()>>,
}

impl EnvVarGuard {
    pub fn set(key: &str, value: Option<&str>) -> Self {
        Self::set_os(key, value.map(OsStr::new))
    }

    pub fn set_os(key: &str, value: Option<&OsStr>) -> Self {
        let lock = ENV_LOCK_DEPTH.with(|depth| {
            if depth.get() == 0 {
                let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
                depth.set(1);
                Some(guard)
            } else {
                depth.set(depth.get() + 1);
                None
            }
        });

        let previous = std::env::var_os(key);
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        Self { key: key.to_string(), previous, _lock: lock }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(&self.key, previous);
        } else {
            std::env::remove_var(&self.key);
        }

        if self._lock.is_some() {
            ENV_LOCK_DEPTH.with(|depth| depth.set(0));
        } else {
            ENV_LOCK_DEPTH.with(|depth| {
                let current = depth.get();
                if current > 0 {
                    depth.set(current - 1);
                }
            });
        }
    }
}
