use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    #[arg(
        long,
        value_name = "TEMPLATE_ID",
        help = "Project template id to initialize from the default template registry.",
        conflicts_with_all = ["path", "walkthrough"]
    )]
    pub(crate) template: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Local template directory containing template.toml.",
        conflicts_with_all = ["template", "walkthrough"]
    )]
    pub(crate) path: Option<String>,
    #[arg(long, help = "Run without prompts. Requires --template, --path, or --walkthrough.")]
    pub(crate) non_interactive: bool,
    #[arg(long, help = "Preview init changes without writing project files.")]
    pub(crate) plan: bool,
    #[arg(long, help = "Overwrite existing project files targeted by the template.")]
    pub(crate) force: bool,
    #[arg(
        long = "update-registry",
        help = "Fetch the latest commit from the template registry and re-pin the local cache before loading the template."
    )]
    pub(crate) update_registry: bool,

    // ---------------------------------------------------------------
    // v0.4.13 onboarding walkthrough flags. These drive the
    // "5 min to first agentic run" experience and are mutually
    // exclusive with the registry-driven --template / --path flow.
    // ---------------------------------------------------------------
    #[arg(
        long,
        help = "Run the onboarding walkthrough: detect CLIs, install default plugins, copy the hello-world workflow.",
        conflicts_with_all = ["template", "path"]
    )]
    pub(crate) walkthrough: bool,
    #[arg(
        long = "no-install",
        help = "Walkthrough only: skip `animus plugin install-defaults`. Use when plugins are already installed."
    )]
    pub(crate) no_install: bool,
    #[arg(
        long = "install-packs",
        help = "Install and activate the recommended workflow packs (animus.core-skills, animus.task, animus.requirement, animus.review) from their pinned GitHub releases. In the interactive walkthrough this is offered automatically (default yes); pass the flag to opt in from non-interactive runs."
    )]
    pub(crate) install_packs: bool,
    #[arg(
        long = "no-template",
        help = "Walkthrough only: skip copying the hello-world workflow template into .animus/workflows/."
    )]
    pub(crate) no_template: bool,
    #[arg(
        long = "auto-start",
        help = "Walkthrough only: start the autonomous daemon after init completes (skipped in non-interactive mode unless set)."
    )]
    pub(crate) auto_start: bool,
    #[arg(
        long = "walkthrough-template",
        value_name = "NAME",
        default_value = "hello-world",
        help = "Walkthrough only: which bundled workflow template to copy. Currently only `hello-world` is shipped."
    )]
    pub(crate) walkthrough_template: String,
}
