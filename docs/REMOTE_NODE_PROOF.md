# Remote Node Proof

This change was authored by an autonomous agent running the Claude Code harness inside an ephemeral Railway node provisioned by the Animus environment plugin.

In practice, that means no human typed these edits at a local workstation. The Animus environment plugin spun up a fresh, disposable Railway compute node on demand, cloned the repository into it, and ran the Claude Code agent harness there. The agent then made this change end to end — reading the repo, editing files, and committing — inside that remote sandbox. Because the node is ephemeral, it exists only for the duration of the work and is torn down afterward, leaving behind just the committed result as evidence that the run happened.
