pub use orchestrator_providers::builtin;
pub use orchestrator_providers::git;

pub use git::BuiltinGitProvider;
pub use orchestrator_providers::{
    BuiltinProjectAdapter, BuiltinRequirementsProvider, BuiltinSubjectResolver, BuiltinTaskProvider, CreatePrInput,
    GitProvider, MergeResult, ProjectAdapter, PullRequestInfo, RequirementsProvider, SubjectContext,
    SubjectResolver, TaskProvider, WorktreeInfo,
};
