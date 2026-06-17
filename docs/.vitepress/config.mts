import { defineConfig } from 'vitepress'
import { withMermaid } from 'vitepress-plugin-mermaid'

export default withMermaid(
  defineConfig({
    title: 'Animus',
    description: 'Agent Orchestrator — orchestrate AI agent workflows from the command line',
    // Empty for Vercel (root). Set VITEPRESS_BASE=/ao-cli/ for GitHub Pages legacy path.
    base: process.env.VITEPRESS_BASE || '/',

    // Hand-drawn, brand-matched mermaid diagrams (electric orange on dark surfaces).
    mermaid: {
      look: 'handDrawn',
      theme: 'base',
      fontFamily: '"Space Grotesk", system-ui, sans-serif',
      themeVariables: {
        primaryColor: '#1c1c1c',
        primaryBorderColor: '#ff7a00',
        primaryTextColor: '#f5f5f5',
        secondaryColor: '#262626',
        tertiaryColor: '#161616',
        lineColor: '#ff9233',
        textColor: '#f5f5f5',
        fontFamily: '"Space Grotesk", system-ui, sans-serif',
        noteBkgColor: '#2a2110',
        noteTextColor: '#f5f5f5',
        noteBorderColor: '#ff7a00',
      },
    },

    head: [
      ['link', { rel: 'icon', type: 'image/svg+xml', href: '/logo.svg' }],
      ['link', { rel: 'preconnect', href: 'https://fonts.googleapis.com' }],
      ['link', { rel: 'preconnect', href: 'https://fonts.gstatic.com', crossorigin: '' }],
      ['link', { rel: 'stylesheet', href: 'https://fonts.googleapis.com/css2?family=Archivo+Black&family=Space+Grotesk:wght@400;500;600;700&family=Plus+Jakarta+Sans:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500&display=swap' }],
    ],

    appearance: 'dark',
    lastUpdated: true,
    ignoreDeadLinks: [
      /\.\.\/\.\.\/crates\//,
      // CHANGELOG lives at repo root (../../CHANGELOG from docs subpages).
      // Linked from SHIP doc, CLI reference, install, migration pages.
      /\.\.\/.*CHANGELOG/,
      // Migration index does not exist yet (each per-version migration page is self-contained).
      /\.\.\/migration\/index/,
    ],

    themeConfig: {
      logo: '/logo.svg',
      siteTitle: 'Animus',

      nav: [
        { text: 'Getting Started', link: '/getting-started/' },
        { text: 'Concepts', link: '/concepts/' },
        { text: 'Guides', link: '/guides/' },
        {
          text: 'Reference',
          items: [
            { text: 'CLI Commands', link: '/reference/cli/' },
            { text: 'Workflow YAML', link: '/reference/workflow-yaml' },
            { text: 'MCP Tools', link: '/reference/mcp-tools' },
            { text: 'Configuration', link: '/reference/configuration' },
          ],
        },
      ],

      sidebar: [
        {
          text: 'Getting Started',
          collapsed: false,
          items: [
            { text: 'Overview', link: '/getting-started/' },
            { text: 'Installation', link: '/getting-started/installation' },
            { text: 'Quick Start', link: '/getting-started/quick-start' },
            { text: 'Project Setup', link: '/getting-started/project-setup' },
            { text: 'A Typical Day', link: '/getting-started/typical-day' },
          ],
        },
        {
          text: 'Concepts',
          collapsed: false,
          items: [
            { text: 'Overview', link: '/concepts/' },
            { text: 'How Animus Works', link: '/concepts/how-ao-works' },
            { text: 'Workflows', link: '/concepts/workflows' },
            { text: 'Subject Dispatch', link: '/concepts/subject-dispatch' },
            { text: 'The Daemon', link: '/concepts/daemon' },
            { text: 'Agents & Phases', link: '/concepts/agents-and-phases' },
            { text: 'MCP Tools', link: '/concepts/mcp-tools' },
            { text: 'State Management', link: '/concepts/state-management' },
            { text: 'Worktrees', link: '/concepts/worktrees' },
          ],
        },
        {
          text: 'Guides',
          collapsed: false,
          items: [
            { text: 'Overview', link: '/guides/' },
            { text: 'Task Management', link: '/guides/task-management' },
            { text: 'Requirements Workflow', link: '/guides/requirements-workflow' },
            { text: 'Writing Workflows', link: '/guides/writing-workflows' },
            { text: 'Daemon Operations', link: '/guides/daemon-operations' },
            { text: 'Operator Runbook', link: '/guides/operator-runbook' },
            { text: 'Model Routing', link: '/guides/model-routing' },
            { text: 'Web Dashboard', link: '/guides/web-dashboard' },
            { text: 'Self-Hosting', link: '/guides/self-hosting' },
            { text: 'Deployment (Docker / Railway / API)', link: '/guides/deployment' },
            { text: 'Plugin Author Guide', link: '/guides/plugin-author-guide' },
            { text: 'CI/CD', link: '/guides/ci-cd' },
            { text: 'Upgrading', link: '/guides/upgrading' },
            { text: 'Troubleshooting', link: '/guides/troubleshooting' },
          ],
        },
        {
          text: 'Reference',
          collapsed: true,
          items: [
            { text: 'Overview', link: '/reference/' },
            { text: 'CLI Commands', link: '/reference/cli/' },
            { text: 'Global Flags', link: '/reference/cli/global-flags' },
            { text: 'Exit Codes', link: '/reference/cli/exit-codes' },
            { text: 'Workflow YAML Schema', link: '/reference/workflow-yaml' },
            { text: 'MCP Tools', link: '/reference/mcp-tools' },
            { text: 'JSON Envelope', link: '/reference/json-envelope' },
            { text: 'Configuration', link: '/reference/configuration' },
            { text: 'Data Layout', link: '/reference/data-layout' },
            { text: 'Status Values', link: '/reference/status-values' },
          ],
        },
        {
          text: 'Architecture',
          collapsed: true,
          items: [
            { text: 'Overview', link: '/architecture/' },
            { text: 'Animus Chat', link: '/architecture/animus-chat' },
            { text: 'Kernel and Flavors', link: '/architecture/kernel-and-flavors' },
            { text: 'Kernel Extraction v0.5', link: '/architecture/kernel-extraction-v0.5' },
            { text: 'v0.5 Protocol Specs', link: '/architecture/v0.5-protocol-specs' },
            { text: 'Animus vs DBOS Transact', link: '/architecture/animus-vs-dbos-transact' },
            { text: 'Naming Contract', link: '/architecture/naming-contract' },
            { text: 'Crate Map', link: '/architecture/crate-map' },
            { text: 'ServiceHub Pattern', link: '/architecture/service-hub' },
            { text: 'Subject Dispatch Daemon', link: '/architecture/subject-dispatch-daemon' },
            { text: 'Tool-Driven Mutation', link: '/architecture/tool-driven-mutation-surfaces' },
            { text: 'Workflow-First CLI', link: '/architecture/workflow-first-cli' },
          ],
        },
        {
          text: 'Internals',
          collapsed: true,
          items: [
            { text: 'Overview', link: '/internals/' },
            { text: 'Daemon Scheduler', link: '/internals/daemon-scheduler' },
            { text: 'Workflow Runner', link: '/internals/workflow-runner' },
            { text: 'Provider Sessions', link: '/internals/agent-runner-ipc' },
            { text: 'State Machines', link: '/internals/state-machines' },
            { text: 'Persistence', link: '/internals/persistence' },
          ],
        },
        {
          text: 'Contributing',
          collapsed: true,
          items: [
            { text: 'Overview', link: '/contributing/' },
            { text: 'Development', link: '/contributing/development' },
            { text: 'Testing', link: '/contributing/testing' },
            { text: 'Dependency Policy', link: '/contributing/dependency-policy' },
          ],
        },
      ],

      socialLinks: [
        { icon: 'github', link: 'https://github.com/launchapp-dev/animus-cli' },
      ],

      footer: {
        message: 'Released under the Elastic License 2.0 (ELv2).',
        copyright: 'Copyright 2024-present LaunchApp / Sami Shukri',
      },

      editLink: {
        pattern: 'https://github.com/launchapp-dev/animus-cli/edit/main/docs/:path',
        text: 'Edit this page on GitHub',
      },

      search: {
        provider: 'local',
      },

      outline: {
        level: [2, 3],
        label: 'On this page',
      },

      lastUpdated: {
        text: 'Last updated',
      },
    },
  })
)
