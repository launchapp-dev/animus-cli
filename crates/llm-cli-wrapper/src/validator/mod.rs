//! CLI output validation

use crate::cli::interface::CliOutput;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ValidationRule {
    ExitCodeEquals(i32),
    OutputContains(String),
    OutputMatches(String), // regex
    StderrEmpty,
    FilesModified(Vec<std::path::PathBuf>),
    DurationLessThan(u64), // milliseconds
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub passed: bool,
    pub failures: Vec<String>,
}

impl ValidationResult {
    pub fn success() -> Self {
        Self { passed: true, failures: Vec::new() }
    }

    pub fn failure(failures: Vec<String>) -> Self {
        Self { passed: false, failures }
    }
}

pub struct CliValidator {
    rules: Vec<ValidationRule>,
}

impl CliValidator {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn validate(&self, output: &CliOutput) -> ValidationResult {
        let mut failures = Vec::new();

        for rule in &self.rules {
            match rule {
                ValidationRule::ExitCodeEquals(expected) => {
                    if output.exit_code != Some(*expected) {
                        failures.push(format!("Expected exit code {} but got {:?}", expected, output.exit_code));
                    }
                }
                ValidationRule::OutputContains(expected) => {
                    if !output.stdout.contains(expected) && !output.stderr.contains(expected) {
                        failures.push(format!("Output doesn't contain '{}'", expected));
                    }
                }
                ValidationRule::OutputMatches(pattern) => {
                    if let Ok(re) = regex::Regex::new(pattern) {
                        if !re.is_match(&output.stdout) && !re.is_match(&output.stderr) {
                            failures.push(format!("Output doesn't match pattern '{}'", pattern));
                        }
                    } else {
                        failures.push(format!("Invalid regex pattern: '{}'", pattern));
                    }
                }
                ValidationRule::StderrEmpty => {
                    if !output.stderr.is_empty() {
                        failures.push(format!("Expected empty stderr but got: {}", output.stderr));
                    }
                }
                ValidationRule::FilesModified(expected_files) => {
                    for file in expected_files {
                        if !file.exists() {
                            failures.push(format!("Expected file {:?} doesn't exist", file));
                        }
                    }
                }
                ValidationRule::DurationLessThan(max_ms) => {
                    if output.duration_ms > *max_ms {
                        failures.push(format!("Duration {}ms exceeds limit of {}ms", output.duration_ms, max_ms));
                    }
                }
            }
        }

        if failures.is_empty() {
            ValidationResult::success()
        } else {
            ValidationResult::failure(failures)
        }
    }
}

impl Default for CliValidator {
    fn default() -> Self {
        Self::new()
    }
}
