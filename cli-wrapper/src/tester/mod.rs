//! CLI testing framework

pub mod test_runner;
pub mod test_suite;

pub use test_runner::CliTester;
pub use test_suite::{TestCase, TestResult, TestSuite};
