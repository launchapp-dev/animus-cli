//! Test suite definitions for CLI testing

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::cli::CliType;

/// A test case for a CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub expected_files: Vec<PathBuf>,
    pub expected_output_contains: Vec<String>,
    pub should_succeed: bool,
    pub timeout_secs: u64,
}

impl TestCase {
    pub fn new(name: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            prompt: prompt.into(),
            expected_files: Vec::new(),
            expected_output_contains: Vec::new(),
            should_succeed: true,
            timeout_secs: 60,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn expect_file(mut self, file: PathBuf) -> Self {
        self.expected_files.push(file);
        self
    }

    pub fn expect_output(mut self, text: impl Into<String>) -> Self {
        self.expected_output_contains.push(text.into());
        self
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

/// Result of running a test case
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub test_name: String,
    pub cli_type: CliType,
    pub passed: bool,
    pub duration_ms: u64,
    pub output: String,
    pub error: Option<String>,
    pub failures: Vec<String>,
}

impl TestResult {
    pub fn success(test_name: String, cli_type: CliType, duration_ms: u64, output: String) -> Self {
        Self { test_name, cli_type, passed: true, duration_ms, output, error: None, failures: Vec::new() }
    }

    pub fn failure(test_name: String, cli_type: CliType, duration_ms: u64, failures: Vec<String>) -> Self {
        Self { test_name, cli_type, passed: false, duration_ms, output: String::new(), error: None, failures }
    }

    pub fn error(test_name: String, cli_type: CliType, error: String) -> Self {
        Self {
            test_name,
            cli_type,
            passed: false,
            duration_ms: 0,
            output: String::new(),
            error: Some(error),
            failures: Vec::new(),
        }
    }
}

/// A suite of tests for CLIs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSuite {
    pub name: String,
    pub description: String,
    pub test_cases: Vec<TestCase>,
}

impl TestSuite {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), description: String::new(), test_cases: Vec::new() }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn add_test(mut self, test: TestCase) -> Self {
        self.test_cases.push(test);
        self
    }

    /// Create a standard test suite for basic CLI verification
    pub fn basic_verification() -> Self {
        Self::new("Basic CLI Verification")
            .with_description("Tests basic CLI functionality and availability")
            .add_test(
                TestCase::new("simple_greeting", "Say hello")
                    .with_description("Simple greeting test")
                    .expect_output("hello")
                    .with_timeout(60),
            )
            .add_test(
                TestCase::new("simple_math", "What is 2 + 2?")
                    .with_description("Simple math question")
                    .expect_output("4")
                    .with_timeout(60),
            )
    }

    /// Create a test suite for file operations
    pub fn file_operations() -> Self {
        Self::new("File Operations")
            .with_description("Tests file reading, writing, and editing")
            .add_test(
                TestCase::new("read_file", "Read the contents of test.txt")
                    .with_description("Test file reading")
                    .with_timeout(30),
            )
            .add_test(
                TestCase::new("write_file", "Create a file named output.txt with 'Test content'")
                    .with_description("Test file writing")
                    .expect_file(PathBuf::from("output.txt"))
                    .with_timeout(30),
            )
    }

    /// Create a test suite for code generation
    pub fn code_generation() -> Self {
        Self::new("Code Generation").with_description("Tests code generation capabilities").add_test(
            TestCase::new("simple_function", "Create a function that adds two numbers in Python")
                .with_description("Generate a simple Python function")
                .expect_output("def")
                .with_timeout(60),
        )
    }
}
