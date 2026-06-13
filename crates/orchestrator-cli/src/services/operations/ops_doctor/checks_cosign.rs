//! Cosign signature presence + verification for installed plugins.
//!
//! Surfaces both "cosign isn't installed at all" and "this plugin has no
//! matching .bundle next to its binary". We do NOT shell out to
//! `cosign verify-blob` here — that requires the trust anchors the install
//! flow uses and is too heavy for a default doctor run. Instead we point at
//! `animus plugin install <repo>@<version>` which re-downloads + re-verifies.

use std::path::PathBuf;

use orchestrator_plugin_host::discover_plugins;

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "cosign";

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();

    let cosign_path = which::which("cosign").ok();
    out.push(match cosign_path.as_ref() {
        Some(path) => DiagnosticCheck::new("cosign_installed", CATEGORY, CheckStatus::Pass, "cosign on PATH")
            .details(format!("cosign found at {}", path.display())),
        None => DiagnosticCheck::new("cosign_installed", CATEGORY, CheckStatus::Warn, "cosign on PATH")
            .current("cosign not found".to_string())
            .expected("cosign resolvable via `which cosign`".to_string())
            .fix(CheckFix::command(
                "install_cosign",
                "Install Sigstore cosign to verify plugin signatures.",
                "brew install cosign",
            )),
    });

    let discovered = match discover_plugins(ctx.project_root.clone()) {
        Ok(list) => list,
        Err(_) => return out,
    };

    // Collapse per-plugin "no .bundle" warnings into a single summary warn.
    // On a default install (~7 plugins) every plugin lacks a co-located
    // signature bundle, which previously emitted one warn per plugin — pure
    // noise. We surface the count plus the two remediation paths instead.
    let mut missing: Vec<String> = Vec::new();
    let mut present = 0usize;
    for plugin in &discovered {
        let bundle_path: PathBuf = plugin.path.with_extension("bundle");
        if bundle_path.exists() {
            present += 1;
        } else {
            missing.push(plugin.name.clone());
        }
    }

    if missing.is_empty() {
        if present > 0 {
            out.push(
                DiagnosticCheck::new("cosign_bundles_present", CATEGORY, CheckStatus::Pass, "Plugin signature bundles")
                    .details(format!("all {present} installed plugin(s) have a co-located signature bundle")),
            );
        }
    } else {
        out.push(
            DiagnosticCheck::new("cosign_bundles_present", CATEGORY, CheckStatus::Warn, "Plugin signature bundles")
                .current(format!(
                    "{} plugin(s) installed without signature bundles: {}",
                    missing.len(),
                    summarize_names(&missing),
                ))
                .expected("matching .bundle file next to each plugin binary".to_string())
                .fix(CheckFix::manual(
                    "reinstall_plugins_signed",
                    &format!(
                        "{} plugins installed without signature bundles; reinstall to fetch them \
                         (`animus plugin install <repo>@latest`).",
                        missing.len(),
                    ),
                )),
        );
    }

    out
}

fn summarize_names(names: &[String]) -> String {
    if names.len() <= 3 {
        names.join(", ")
    } else {
        format!("{}, … (+{} more)", names[..3].join(", "), names.len() - 3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn summarize_lists_all_when_three_or_fewer() {
        assert_eq!(summarize_names(&names(&["a", "b"])), "a, b");
        assert_eq!(summarize_names(&names(&["a", "b", "c"])), "a, b, c");
    }

    #[test]
    fn summarize_truncates_beyond_three() {
        assert_eq!(summarize_names(&names(&["a", "b", "c", "d", "e"])), "a, b, c, … (+2 more)");
    }

    #[test]
    fn summarize_empty_is_blank() {
        assert_eq!(summarize_names(&[]), "");
    }
}
