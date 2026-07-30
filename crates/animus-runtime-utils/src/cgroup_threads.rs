//! Compute the Tokio worker-thread count from the cgroup CPU quota so
//! binaries don't default to the host CPU count when running inside a
//! resource-capped container.
//!
//! Priority:
//!   1. `TOKIO_WORKER_THREADS` env var (explicit operator override)
//!   2. cgroup v2 `cpu.max` quota (`/sys/fs/cgroup/cpu.max`)
//!   3. cgroup v1 quota (`/sys/fs/cgroup/cpu/cpu.cfs_quota_us`)
//!   4. `std::thread::available_parallelism()` (host-visible CPU count)
//!
//! The result is always >= 1.
//!
//! This module lives in the dependency-light `animus-runtime-utils` crate so
//! every in-workspace runtime entry point can use it without dependency cycles.

/// Return the number of Tokio worker threads appropriate for this process,
/// honouring the cgroup CPU quota when present.
///
/// Pass the return value to `tokio::runtime::Builder::worker_threads` instead
/// of relying on `#[tokio::main]`'s default (which uses the host CPU count,
/// not the container quota).
pub fn tokio_worker_threads() -> usize {
    read_env_override().or_else(cgroup_v2_threads).or_else(cgroup_v1_threads).unwrap_or_else(system_threads).max(1)
}

fn read_env_override() -> Option<usize> {
    let s = std::env::var("TOKIO_WORKER_THREADS").ok()?;
    parse_env_override(s.trim())
}

/// Parse a `TOKIO_WORKER_THREADS` value; returns `None` for zero or non-numeric.
pub(crate) fn parse_env_override(s: &str) -> Option<usize> {
    s.parse::<usize>().ok().filter(|&n| n > 0)
}

fn cgroup_v2_threads() -> Option<usize> {
    let content = std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok()?;
    parse_cgroup_v2(&content)
}

/// Parse a cgroup v2 `cpu.max` file content (`<quota_us> <period_us>`).
///
/// Returns `None` when the quota is unlimited (`"max"`) or the file is malformed.
pub(crate) fn parse_cgroup_v2(content: &str) -> Option<usize> {
    let mut parts = content.split_whitespace();
    let quota_str = parts.next()?;
    let period_str = parts.next()?;
    if parts.next().is_some() || quota_str == "max" {
        return None;
    }
    let quota: u64 = quota_str.parse().ok()?;
    let period: u64 = period_str.parse().ok()?;
    if quota == 0 || period == 0 {
        return None;
    }
    Some(quota.div_ceil(period).try_into().unwrap_or(usize::MAX).max(1))
}

fn cgroup_v1_threads() -> Option<usize> {
    // The CPU controller may be mounted directly at the cgroup root or in a
    // named controller directory, depending on the container runtime.
    const CONTROLLER_PATHS: &[&str] = &["/sys/fs/cgroup", "/sys/fs/cgroup/cpu", "/sys/fs/cgroup/cpu,cpuacct"];

    CONTROLLER_PATHS.iter().find_map(|controller_path| {
        let quota_path = format!("{controller_path}/cpu.cfs_quota_us");
        let period_path = format!("{controller_path}/cpu.cfs_period_us");
        let quota_str = std::fs::read_to_string(quota_path).ok()?;
        let period_str = std::fs::read_to_string(period_path).ok()?;
        parse_cgroup_v1(quota_str.trim(), period_str.trim())
    })
}

/// Parse cgroup v1 quota/period strings from `cpu.cfs_quota_us` / `cpu.cfs_period_us`.
///
/// Returns `None` when the quota is unlimited (`-1`) or either value is malformed.
pub(crate) fn parse_cgroup_v1(quota_str: &str, period_str: &str) -> Option<usize> {
    let quota: i64 = quota_str.parse().ok()?;
    if quota < 0 {
        return None; // -1 = unlimited
    }
    let period: u64 = period_str.parse().ok()?;
    if quota == 0 || period == 0 {
        return None;
    }
    Some((quota as u64).div_ceil(period).try_into().unwrap_or(usize::MAX).max(1))
}

fn system_threads() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_threads_is_at_least_one() {
        assert!(system_threads() >= 1);
    }

    #[test]
    fn result_is_at_least_one() {
        assert!(tokio_worker_threads() >= 1);
    }

    #[test]
    fn env_override_zero_is_rejected() {
        assert!(parse_env_override("0").is_none());
    }

    #[test]
    fn env_override_positive_is_accepted() {
        assert_eq!(parse_env_override("4"), Some(4));
    }

    #[test]
    fn env_override_overflow_is_rejected() {
        assert!(parse_env_override("999999999999999999999999999999999999999").is_none());
    }

    #[test]
    fn env_override_invalid_is_rejected() {
        assert!(parse_env_override("abc").is_none());
    }

    #[test]
    fn cgroup_v2_unlimited_returns_none() {
        assert!(parse_cgroup_v2("max 100000").is_none());
    }

    #[test]
    fn cgroup_v2_two_cpus() {
        // 200_000 quota / 100_000 period = 2 CPUs
        assert_eq!(parse_cgroup_v2("200000 100000"), Some(2));
    }

    #[test]
    fn cgroup_v2_sub_cpu_quota_has_one_worker() {
        // 50_000 / 100_000 = 0.5 CPUs → clamped to 1
        assert_eq!(parse_cgroup_v2("50000 100000"), Some(1));
    }

    #[test]
    fn cgroup_v2_fractional_cpu_rounds_up() {
        assert_eq!(parse_cgroup_v2("150000 100000"), Some(2));
    }

    #[test]
    fn cgroup_v2_zero_period_returns_none() {
        assert!(parse_cgroup_v2("200000 0").is_none());
    }

    #[test]
    fn cgroup_v2_zero_quota_returns_none() {
        assert!(parse_cgroup_v2("0 100000").is_none());
    }

    #[test]
    fn cgroup_v2_extra_fields_returns_none() {
        assert!(parse_cgroup_v2("200000 100000 unexpected").is_none());
    }

    #[test]
    fn cgroup_v2_missing_period_returns_none() {
        assert!(parse_cgroup_v2("200000").is_none());
    }

    #[test]
    fn cgroup_v1_unlimited_returns_none() {
        assert!(parse_cgroup_v1("-1", "100000").is_none());
    }

    #[test]
    fn cgroup_v1_two_cpus() {
        assert_eq!(parse_cgroup_v1("200000", "100000"), Some(2));
    }

    #[test]
    fn cgroup_v1_fractional_cpu_rounds_up() {
        assert_eq!(parse_cgroup_v1("150000", "100000"), Some(2));
    }

    #[test]
    fn cgroup_v1_zero_period_returns_none() {
        assert!(parse_cgroup_v1("200000", "0").is_none());
    }

    #[test]
    fn cgroup_v1_zero_quota_returns_none() {
        assert!(parse_cgroup_v1("0", "100000").is_none());
    }

    #[test]
    fn cgroup_v1_malformed_quota_returns_none() {
        assert!(parse_cgroup_v1("not-a-number", "100000").is_none());
    }
}
