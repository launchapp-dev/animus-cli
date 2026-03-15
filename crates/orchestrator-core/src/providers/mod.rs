pub use orchestrator_providers::builtin;
pub use orchestrator_providers::git;
#[cfg(feature = "gitlab")]
pub use orchestrator_providers::gitlab;
#[cfg(feature = "jira")]
pub use orchestrator_providers::jira;
#[cfg(feature = "linear")]
pub use orchestrator_providers::linear;

pub use git::BuiltinGitProvider;
pub use orchestrator_providers::{
    BuiltinProjectAdapter, BuiltinRequirementsProvider, BuiltinSubjectResolver, BuiltinTaskProvider, CreatePrInput,
    GitHubProvider, GitProvider, MergeResult, ProjectAdapter, PullRequestInfo, RequirementsProvider, SubjectContext,
    SubjectResolver, TaskProvider, WorktreeInfo,
};
#[cfg(feature = "gitlab")]
pub use orchestrator_providers::{GitLabConfig, GitLabGitProvider};
#[cfg(feature = "jira")]
pub use orchestrator_providers::{JiraConfig, JiraTaskProvider};
#[cfg(feature = "linear")]
pub use orchestrator_providers::{LinearConfig, LinearTaskProvider};
