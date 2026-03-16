use std::io::ErrorKind;
use std::process::Command;

use anyhow::{anyhow, Result};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::workflow_config::WorkflowConfig;

use super::loading::LoadedPackManifest;
use super::mcp::apply_pack_mcp_overlay;
use super::types::{ExternalRuntimeKind, PackRuntimeRequirement};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackRuntimeCheckStatus {
    Satisfied,
    MissingOptional,
    MissingRequired,
    VersionMismatchOptional,
    VersionMismatchRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackRuntimeCheck {
    pub runtime: ExternalRuntimeKind,
    pub requested_binary: Option<String>,
    pub resolved_binary: Option<String>,
    pub version_requirement: Option<String>,
    pub detected_version: Option<String>,
    pub status: PackRuntimeCheckStatus,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackRuntimeReport {
    pub checks: Vec<PackRuntimeCheck>,
}

impl PackRuntimeReport {
    pub fn has_required_failures(&self) -> bool {
        self.checks.iter().any(|check| {
            matches!(
                check.status,
                PackRuntimeCheckStatus::MissingRequired | PackRuntimeCheckStatus::VersionMismatchRequired
            )
        })
    }
}

pub fn check_pack_runtime_requirements(pack: &LoadedPackManifest) -> Result<PackRuntimeReport> {
    let mut checks = Vec::new();

    for requirement in &pack.manifest.runtime.requirements {
        checks.push(check_runtime_requirement(&pack.manifest.id, requirement)?);
    }

    Ok(PackRuntimeReport { checks })
}

pub fn ensure_pack_runtime_requirements(pack: &LoadedPackManifest) -> Result<PackRuntimeReport> {
    let report = check_pack_runtime_requirements(pack)?;
    if !report.has_required_failures() {
        return Ok(report);
    }

    let failures = report
        .checks
        .iter()
        .filter(|check| {
            matches!(
                check.status,
                PackRuntimeCheckStatus::MissingRequired | PackRuntimeCheckStatus::VersionMismatchRequired
            )
        })
        .map(|check| check.message.clone())
        .collect::<Vec<_>>();

    Err(anyhow!(failures.join("; ")))
}

pub fn activate_pack_mcp_overlay(
    workflow: &mut WorkflowConfig,
    pack: &LoadedPackManifest,
) -> Result<PackRuntimeReport> {
    let report = crate::ensure_pack_execution_requirements(pack)?;
    apply_pack_mcp_overlay(workflow, pack)?;
    Ok(report)
}

fn check_runtime_requirement(pack_id: &str, requirement: &PackRuntimeRequirement) -> Result<PackRuntimeCheck> {
    let version_req = requirement
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(VersionReq::parse)
        .transpose()
        .map_err(|error| {
            anyhow!(
                "pack '{}' runtime '{}' has invalid version constraint '{}': {}",
                pack_id,
                requirement.runtime.binary_name(),
                requirement.version.as_deref().unwrap_or_default(),
                error
            )
        })?;

    let candidates = runtime_candidates(requirement);
    let mut mismatch_details = Vec::new();

    for candidate in &candidates {
        match probe_runtime(candidate)? {
            None => continue,
            Some(detected_version) => {
                let detected_semver = parse_detected_version(&detected_version);
                if let Some(version_req) = version_req.as_ref() {
                    let Some(detected_semver) = detected_semver else {
                        mismatch_details.push(format!(
                            "'{}' is installed but returned an unreadable version string '{}'",
                            candidate, detected_version
                        ));
                        continue;
                    };
                    if version_req.matches(&detected_semver) {
                        return Ok(PackRuntimeCheck {
                            runtime: requirement.runtime.clone(),
                            requested_binary: requirement.binary.clone(),
                            resolved_binary: Some(candidate.clone()),
                            version_requirement: requirement.version.clone(),
                            detected_version: Some(detected_semver.to_string()),
                            status: PackRuntimeCheckStatus::Satisfied,
                            message: format!(
                                "pack '{}' runtime '{}' satisfied by '{}' ({})",
                                pack_id,
                                requirement.runtime.binary_name(),
                                candidate,
                                detected_semver
                            ),
                        });
                    }
                    mismatch_details.push(format!(
                        "'{}' resolved to version {} which does not satisfy '{}'",
                        candidate, detected_semver, version_req
                    ));
                    continue;
                }

                return Ok(PackRuntimeCheck {
                    runtime: requirement.runtime.clone(),
                    requested_binary: requirement.binary.clone(),
                    resolved_binary: Some(candidate.clone()),
                    version_requirement: requirement.version.clone(),
                    detected_version: detected_semver.map(|value| value.to_string()).or(Some(detected_version)),
                    status: PackRuntimeCheckStatus::Satisfied,
                    message: format!(
                        "pack '{}' runtime '{}' satisfied by '{}'",
                        pack_id,
                        requirement.runtime.binary_name(),
                        candidate
                    ),
                });
            }
        }
    }

    let required = !requirement.optional;
    let status = if mismatch_details.is_empty() {
        if required {
            PackRuntimeCheckStatus::MissingRequired
        } else {
            PackRuntimeCheckStatus::MissingOptional
        }
    } else if required {
        PackRuntimeCheckStatus::VersionMismatchRequired
    } else {
        PackRuntimeCheckStatus::VersionMismatchOptional
    };

    let message = if mismatch_details.is_empty() {
        format!(
            "pack '{}' requires runtime '{}' but no executable was found (tried: {})",
            pack_id,
            requirement.runtime.binary_name(),
            candidates.join(", ")
        )
    } else {
        format!(
            "pack '{}' runtime '{}' is not compatible: {}",
            pack_id,
            requirement.runtime.binary_name(),
            mismatch_details.join("; ")
        )
    };

    Ok(PackRuntimeCheck {
        runtime: requirement.runtime.clone(),
        requested_binary: requirement.binary.clone(),
        resolved_binary: None,
        version_requirement: requirement.version.clone(),
        detected_version: None,
        status,
        message,
    })
}

fn runtime_candidates(requirement: &PackRuntimeRequirement) -> Vec<String> {
    if let Some(binary) = requirement.binary.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        return vec![binary.to_string()];
    }

    match requirement.runtime {
        ExternalRuntimeKind::Python => vec!["python".to_string(), "python3".to_string()],
        _ => vec![requirement.runtime.binary_name().to_string()],
    }
}

fn probe_runtime(binary: &str) -> Result<Option<String>> {
    let output = match Command::new(binary).arg("--version").output() {
        Ok(output) => output,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(anyhow!("failed to execute runtime probe '{} --version': {}", binary, error)),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{} {}", stdout.trim(), stderr.trim()).trim().to_string();

    if combined.is_empty() && !output.status.success() {
        return Ok(Some(format!("exit-status-{}", output.status)));
    }

    Ok(Some(combined))
}

fn parse_detected_version(raw: &str) -> Option<Version> {
    let mut started = false;
    let mut captured = String::new();

    for ch in raw.chars() {
        if ch.is_ascii_digit() {
            started = true;
            captured.push(ch);
            continue;
        }

        if started && ch == '.' {
            captured.push(ch);
            continue;
        }

        if started {
            break;
        }
    }

    let trimmed = captured.trim_matches('.');
    if trimmed.is_empty() {
        return None;
    }

    let mut segments: Vec<&str> = trimmed.split('.').filter(|segment| !segment.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }
    while segments.len() < 3 {
        segments.push("0");
    }

    Version::parse(&segments[..3].join(".")).ok()
}
