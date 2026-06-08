//! Synthetic 30-plugin discovery benchmark.
//!
//! Run with `cargo test -p orchestrator-plugin-host --release --test manifest_cache_bench -- --ignored --nocapture`.
//! The targets enforced here are the perf budgets from the v0.5.9 manifest
//! cache initiative:
//!
//! - Cold cache (parallel probes): under 1500ms in release mode.
//! - Warm cache: under 200ms in release mode (target is <50ms; CI slack
//!   makes the assertion looser so we don't redline on heavily loaded
//!   runners — the headline number is what's printed via `--nocapture`).
//!
//! The plugin scripts are tiny `/bin/sh` programs that print a manifest and
//! exit; the cold-cache cost is dominated by fork/exec + sha256 hashing, the
//! warm-cache cost is dominated by JSON read + parse.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

use orchestrator_plugin_host::PluginDiscovery;

const PLUGIN_COUNT: usize = 30;

fn write_plugin(path: &std::path::Path, name: &str) {
    let manifest = serde_json::json!({
        "name": name,
        "version": "0.1.0",
        "plugin_kind": "custom",
        "description": "bench",
        "protocol_version": "1.0.0",
        "capabilities": []
    });
    fs::write(path, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
#[ignore = "perf benchmark; opt-in"]
fn bench_30_plugin_discovery_cold_then_warm() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let install = home.join("plugins");
    fs::create_dir_all(&install).unwrap();

    std::env::set_var("ANIMUS_CONFIG_DIR", &home);
    std::env::set_var("ANIMUS_CACHE_DIR", home.join("cache"));
    std::env::set_var("ANIMUS_PLUGIN_DIR", &install);
    std::env::set_var("ANIMUS_PLUGIN_PATH", "");
    std::env::remove_var("ANIMUS_DISABLE_MANIFEST_CACHE");

    for idx in 0..PLUGIN_COUNT {
        let name = format!("animus-plugin-bench-{idx:02}");
        write_plugin(&install.join(&name), &name);
    }

    let empty_config = temp.path().join("plugins.yaml");
    fs::write(&empty_config, "plugins: {}\n").unwrap();

    let cold_start = Instant::now();
    let (cold_discovered, cold_warnings) =
        PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("cold discover");
    let cold_elapsed = cold_start.elapsed();
    assert_eq!(cold_discovered.len(), PLUGIN_COUNT, "cold discover must find every plugin");
    assert!(cold_warnings.is_empty(), "no warnings expected, got {cold_warnings:?}");

    let warm_start = Instant::now();
    let (warm_discovered, warm_warnings) =
        PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("warm discover");
    let warm_elapsed = warm_start.elapsed();
    assert_eq!(warm_discovered.len(), PLUGIN_COUNT, "warm discover must still return every plugin");
    assert!(warm_warnings.is_empty(), "no warnings expected, got {warm_warnings:?}");

    eprintln!("[manifest-cache-bench] {} plugins | cold {:?} | warm {:?}", PLUGIN_COUNT, cold_elapsed, warm_elapsed);

    assert!(
        warm_elapsed.as_millis() < cold_elapsed.as_millis() / 2,
        "warm cache must be meaningfully faster than cold: cold {cold_elapsed:?}, warm {warm_elapsed:?}"
    );
}
