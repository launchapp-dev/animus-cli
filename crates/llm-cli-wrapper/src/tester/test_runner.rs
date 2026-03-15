//! CLI test runner

use std::sync::Arc;
use tracing::{info, warn};

use super::test_suite::{TestCase, TestResult, TestSuite};
use crate::cli::interface::CliCommand;
use crate::cli::{CliInterface, CliRegistry};
use crate::error::Result;

pub struct CliTester {
    temp_dir: Option<std::path::PathBuf>,
}

impl CliTester {
    pub fn new() -> Self {
        Self { temp_dir: None }
    }

    pub fn with_temp_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.temp_dir = Some(dir);
        self
    }

    /// Test all CLIs in the registry with the given test suite
    pub async fn test_all_clis(&self, registry: &CliRegistry, suite: &TestSuite) -> Result<Vec<TestResult>> {
        let mut results = Vec::new();

        for cli in registry.all() {
            info!("Testing {} with suite '{}'", cli.metadata().cli_type.display_name(), suite.name);

            for test_case in &suite.test_cases {
                let result = self.run_test(cli.clone(), test_case).await;
                results.push(result);
            }
        }

        Ok(results)
    }

    /// Test a specific CLI with a test suite
    pub async fn test_cli(&self, cli: Arc<dyn CliInterface>, suite: &TestSuite) -> Result<Vec<TestResult>> {
        let mut results = Vec::new();

        for test_case in &suite.test_cases {
            let result = self.run_test(cli.clone(), test_case).await;
            results.push(result);
        }

        Ok(results)
    }

    /// Run a single test case
    async fn run_test(&self, cli: Arc<dyn CliInterface>, test: &TestCase) -> TestResult {
        let start = std::time::Instant::now();
        let cli_type = cli.metadata().cli_type;

        info!("Running test: {}", test.name);

        // Create command
        let mut command = CliCommand::new(test.prompt.clone()).with_timeout(test.timeout_secs);

        if let Some(ref dir) = self.temp_dir {
            command = command.with_working_dir(dir.clone());
        }

        // Execute
        match cli.execute(&command).await {
            Ok(output) => {
                let duration_ms = start.elapsed().as_millis() as u64;

                // Validate results
                let mut failures = Vec::new();

                // Check exit code
                if test.should_succeed && !output.is_success() {
                    failures.push(format!("Expected success but got exit code: {:?}", output.exit_code));
                }

                // Check expected output (case-insensitive)
                for expected in &test.expected_output_contains {
                    let stdout_lower = output.stdout.to_lowercase();
                    let stderr_lower = output.stderr.to_lowercase();
                    let expected_lower = expected.to_lowercase();

                    if !stdout_lower.contains(&expected_lower) && !stderr_lower.contains(&expected_lower) {
                        failures.push(format!("Expected output to contain '{}' but it didn't", expected));
                    }
                }

                // Check expected files
                for expected_file in &test.expected_files {
                    if !expected_file.exists() {
                        failures.push(format!("Expected file {:?} to exist but it doesn't", expected_file));
                    }
                }

                if failures.is_empty() {
                    TestResult::success(test.name.clone(), cli_type, duration_ms, output.stdout)
                } else {
                    TestResult::failure(test.name.clone(), cli_type, duration_ms, failures)
                }
            }
            Err(e) => {
                warn!("Test '{}' failed with error: {}", test.name, e);
                TestResult::error(test.name.clone(), cli_type, e.to_string())
            }
        }
    }

    /// Run a quick health check on a CLI
    pub async fn health_check(&self, cli: Arc<dyn CliInterface>) -> Result<TestResult> {
        let start = std::time::Instant::now();
        let cli_type = cli.metadata().cli_type;

        info!("Running health check for {}", cli_type.display_name());

        // Check if available
        if !cli.is_available().await {
            return Ok(TestResult::error("health_check".to_string(), cli_type, "CLI is not available".to_string()));
        }

        // Check auth
        match cli.check_auth().await {
            Ok(true) => {}
            Ok(false) => {
                return Ok(TestResult::error(
                    "health_check".to_string(),
                    cli_type,
                    "CLI is not authenticated".to_string(),
                ));
            }
            Err(e) => {
                return Ok(TestResult::error(
                    "health_check".to_string(),
                    cli_type,
                    format!("Auth check failed: {}", e),
                ));
            }
        }

        // Get version
        let version = match cli.get_version().await {
            Ok(v) => v,
            Err(e) => {
                return Ok(TestResult::error(
                    "health_check".to_string(),
                    cli_type,
                    format!("Version check failed: {}", e),
                ));
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(TestResult::success(
            "health_check".to_string(),
            cli_type,
            duration_ms,
            format!("CLI is healthy (version: {})", version),
        ))
    }
}

impl Default for CliTester {
    fn default() -> Self {
        Self::new()
    }
}
