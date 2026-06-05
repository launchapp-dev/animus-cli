<script setup lang="ts">
const models = [
  { name: 'CLAUDE', tag: 'opus', state: 'active' },
  { name: 'CODEX', tag: 'gpt-5', state: 'swap' },
  { name: 'GEMINI', tag: '3-pro', state: 'swap' },
  { name: 'OPENAI', tag: 'o-series', state: 'swap' },
  { name: 'OPENCODE', tag: 'local', state: 'swap' },
  { name: 'OAI-COMPAT', tag: 'any', state: 'swap' },
]

const compare = [
  { label: 'ISOLATE STATE', vals: [false, false, false, true] },
  { label: 'GATE OUTPUT', vals: [false, false, false, true] },
  { label: 'REWORK ON FAILURE', vals: [false, false, false, true] },
  { label: 'SHIP TO MAIN', vals: [false, false, false, true] },
]

const steps = [
  { n: '1', title: 'QUEUE', body: 'Every task lands in its own git worktree.', link: '/concepts/workflows' },
  { n: '2', title: 'ISOLATE', body: 'Phases run sandboxed. Branch conflicts are impossible.', link: '/concepts/worktrees' },
  { n: '3', title: 'CONTRACT', body: 'Typed verdicts: advance · rework · skip · fail.', link: '/concepts/agents-and-phases' },
  { n: '4', title: 'SHIP', body: 'Pass advances. Fail reworks. Done merges to main.', link: '/concepts/daemon' },
]

const surface = [
  { part: 'A.001', tag: 'START HERE', title: 'GETTING STARTED', body: 'Install the binary, create a task, run your first workflow.', meta: '5 min', link: '/getting-started/' },
  { part: 'A.002', tag: 'CONCEPTS', title: 'HOW IT WORKS', body: 'Workflows, subject dispatch, the daemon, agents & phases.', meta: '9 pages', link: '/concepts/' },
  { part: 'A.003', tag: 'GUIDES', title: 'OPERATE IT', body: 'Task management, model routing, daemon ops, CI/CD.', meta: '13 guides', link: '/guides/' },
  { part: 'B.001', tag: 'REFERENCE', title: 'CLI + YAML', body: 'Command tree, workflow schema, MCP tools, config.', meta: 'full spec', link: '/reference/' },
  { part: 'B.002', tag: 'ARCHITECTURE', title: 'KERNEL + FLAVORS', body: 'The load-bearing design: a kernel and a default flavor.', meta: 'deep dive', link: '/architecture/' },
  { part: 'B.003', tag: 'INTERNALS', title: 'UNDER THE HOOD', body: 'Scheduler, workflow runner, state machines, persistence.', meta: 'source-level', link: '/internals/' },
]

const marquee = ['QUEUE · ISOLATE · GATE · SHIP', 'CLAUDE · CODEX · GEMINI · OPENAI', 'DEFINE YOUR TEAM IN YAML']
const installCmd = 'curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash'
</script>

<template>
  <div class="animus-landing">
    <!-- ============================= HERO ============================= -->
    <section class="hero wrap">
      <div class="hero-grid">
        <div class="hero-copy">
          <div class="eyebrow"><span class="bar" />THE RUNTIME FOR THE AGENT ERA</div>
          <h1 class="display">
            Orchestrate AI agents.<br />
            Ship code <span class="accent">autonomously.</span>
          </h1>
          <p class="lede">
            Animus dispatches, isolates, gates, and ships AI agent workflows from a
            single Rust binary. Define your team in YAML. Step away from the terminal.
          </p>
          <div class="cta-row">
            <a class="btn btn-solid" href="/getting-started/">Get Started →</a>
            <a class="btn btn-ghost" href="https://github.com/launchapp-dev/animus-cli" target="_blank" rel="noreferrer">★ Star on GitHub</a>
            <a class="btn btn-ghost" href="/reference/">Read the Docs ↗</a>
          </div>
        </div>

        <!-- terminal demo -->
        <div class="term">
          <div class="term-bar">
            <span class="dot" /><span class="dot" /><span class="dot" />
            <span class="term-title">animus · queue · live</span>
            <span class="term-state">autonomous</span>
          </div>
          <pre class="term-body"><span class="c-prompt">$</span> animus queue <span class="c-str">"fix flaky billing test"</span>
<span class="ok">✓</span> worktree spawned<span class="t">0.4s</span>
<span class="run">◐</span> implement<span class="t">running…  claude</span>
<span class="ok">✓</span> implement<span class="t">4m 12s   claude</span>
<span class="run">◐</span> review<span class="t">running…  codex</span>
<span class="ok">✓</span> review<span class="t">1m 48s   advance</span>
<span class="ok">✓</span> test<span class="t">47s      cargo test → ok</span>
<span class="ok">✓</span> merged to main<span class="t">0.2s</span></pre>
        </div>
      </div>
    </section>

    <!-- ============================= MARQUEE ============================= -->
    <div class="marquee">
      <div class="marquee-track">
        <template v-for="rep in 4" :key="rep">
          <span v-for="(m, i) in marquee" :key="rep + '-' + i" class="marquee-item">
            {{ m }}<span class="star">✱</span>
          </span>
        </template>
      </div>
    </div>

    <!-- ============================= §01 PROBLEM ============================= -->
    <section class="wrap block">
      <div class="block-grid">
        <div class="block-head">
          <div class="sec-no">§01 — THE PROBLEM</div>
          <h2 class="display sm">Frameworks ship tools.<br /><span class="accent">Runtimes ship outcomes.</span></h2>
          <p class="lede">
            Multi-agent frameworks hand you primitives and walk away. None isolate
            state. None gate output. None rework on failure. None ship.
            Animus does all four — by default.
          </p>
        </div>
        <div class="panel cmp">
          <div class="cmp-row cmp-head">
            <div class="cmp-label">REQUIREMENT</div>
            <div class="cmp-cell">FRAMEWORK A</div>
            <div class="cmp-cell">FRAMEWORK B</div>
            <div class="cmp-cell">CODEX MA</div>
            <div class="cmp-cell hot">ANIMUS</div>
          </div>
          <div class="cmp-row" v-for="row in compare" :key="row.label">
            <div class="cmp-label">{{ row.label }}</div>
            <div class="cmp-cell" v-for="(v, i) in row.vals" :key="i" :class="{ hot: i === 3 }">
              <span :class="v ? 'yes' : 'no'">{{ v ? '✓' : '✕' }}</span>
            </div>
          </div>
        </div>
      </div>
    </section>

    <!-- ============================= §02 THESIS ============================= -->
    <section class="wrap block">
      <div class="block-grid">
        <div class="block-head">
          <div class="sec-no">§02 — THE THESIS</div>
          <h2 class="display sm">The model is a <span class="accent">config flag.</span></h2>
          <p class="lede">
            Every model plugs into Animus through the same provider protocol.
            Swap per phase. Swap per quarter. Swap the day a new model ships.
            Workflows and output contracts compound across generations.
          </p>
        </div>
        <div class="model-grid">
          <div class="model-card" v-for="m in models" :key="m.name" :class="{ active: m.state === 'active' }">
            <div class="model-name">{{ m.name }}</div>
            <div class="model-tag">{{ m.tag }}</div>
            <div class="model-state">
              <span v-if="m.state === 'active'" class="bar" />{{ m.state === 'active' ? 'ACTIVE' : '○ swap-ready' }}
            </div>
          </div>
        </div>
      </div>
    </section>

    <!-- ============================= §03 PRODUCT ============================= -->
    <section class="wrap block">
      <div class="sec-no">§03 — THE PRODUCT</div>
      <h2 class="display sm center">Queue. Isolate. <span class="accent">Contract. Ship.</span></h2>
      <div class="steps">
        <a class="step" v-for="s in steps" :key="s.n" :href="s.link">
          <div class="step-n">{{ s.n }}</div>
          <div class="step-title">{{ s.title }}</div>
          <div class="step-body">{{ s.body }}</div>
          <div class="step-link">Learn more →</div>
        </a>
      </div>
    </section>

    <!-- ============================= §04 SURFACE ============================= -->
    <section class="wrap block">
      <div class="sec-no">§04 — THE DOCS</div>
      <h2 class="display sm">Explore the <span class="accent">surface.</span></h2>
      <div class="surface-grid">
        <a class="surface-card" v-for="c in surface" :key="c.part" :href="c.link">
          <div class="surface-top">
            <span class="surface-part">PART {{ c.part }} · {{ c.tag }}</span>
            <span class="surface-meta">{{ c.meta }}</span>
          </div>
          <div class="surface-title">{{ c.title }}</div>
          <div class="surface-body">{{ c.body }}</div>
          <div class="surface-arrow">→</div>
        </a>
      </div>
    </section>

    <!-- ============================= §05 INSTALL ============================= -->
    <section class="wrap block install">
      <div class="sec-no">05 — INSTALL</div>
      <h2 class="display sm">One paste. <span class="accent">Single binary.</span></h2>
      <p class="lede center-narrow">
        Rust-only. No runtime, no containers. You'll be running workflows in about a minute.
      </p>
      <div class="install-box">
        <div class="install-bar"><span class="dot" /><span class="dot" /><span class="dot" /><span class="install-label">install.sh</span></div>
        <pre class="install-cmd"><span class="c-prompt">$</span> {{ installCmd }}</pre>
      </div>
      <div class="install-links">
        <a href="https://github.com/launchapp-dev/animus-cli" target="_blank" rel="noreferrer">★ GitHub →</a>
        <a href="/getting-started/installation">📦 Full install guide →</a>
        <a href="/getting-started/">🚀 Quick start →</a>
      </div>
    </section>
  </div>
</template>

<style scoped>
.animus-landing {
  --hot: var(--vp-c-brand-1, #ff7a00);
  --ink: var(--vp-c-text-1);
  --ink-2: var(--vp-c-text-2);
  --line: var(--vp-c-divider);
  --surface: var(--vp-c-bg-soft);
  color: var(--ink);
  overflow: hidden;
}

.wrap {
  max-width: 1200px;
  margin: 0 auto;
  padding: 0 24px;
}

.display {
  font-family: var(--animus-font-display);
  font-weight: 400;
  letter-spacing: -0.02em;
  line-height: 1.0;
  margin: 0;
  text-transform: none;
}
.display.sm { font-size: clamp(28px, 4.2vw, 52px); }
.accent { color: var(--hot); }
.center { text-align: center; }
.center-narrow { max-width: 540px; margin-left: auto; margin-right: auto; text-align: center; }

.eyebrow,
.sec-no {
  font-family: var(--vp-font-family-mono);
  font-size: 12px;
  letter-spacing: 0.14em;
  color: var(--ink-2);
  display: flex;
  align-items: center;
  gap: 8px;
}
.sec-no { margin-bottom: 18px; }
.bar { display: inline-block; width: 4px; height: 13px; background: var(--hot); }

.lede {
  color: var(--ink-2);
  font-size: 16px;
  line-height: 1.65;
  max-width: 46ch;
}

/* ----------------------------- HERO ----------------------------- */
.hero { padding-top: clamp(48px, 8vw, 96px); padding-bottom: 56px; position: relative; }
.hero-grid {
  display: grid;
  grid-template-columns: 1.05fr 1fr;
  gap: 48px;
  align-items: center;
}
.hero .display {
  font-size: clamp(40px, 6.4vw, 80px);
  margin: 20px 0 22px;
}
.cta-row { display: flex; flex-wrap: wrap; gap: 12px; margin-top: 30px; }
.btn {
  font-family: var(--animus-font-heading);
  font-weight: 600;
  font-size: 14px;
  padding: 12px 20px;
  border-radius: 6px;
  text-decoration: none;
  border: 1px solid transparent;
  transition: transform .15s ease, background .2s ease, border-color .2s ease, color .2s ease;
  white-space: nowrap;
}
.btn:hover { transform: translateY(-2px); }
.btn-solid { background: var(--hot); color: #0a0a0a; }
.btn-solid:hover { background: #ff9233; }
.btn-ghost { border-color: var(--line); color: var(--ink); }
.btn-ghost:hover { border-color: var(--hot); color: var(--hot); }

/* terminal */
.term {
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--vp-c-bg-alt, #000);
  overflow: hidden;
  box-shadow: 0 20px 60px rgba(0,0,0,.35), 0 0 0 1px rgba(255,122,0,.04);
}
.term-bar {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 10px 14px;
  border-bottom: 1px solid var(--line);
  background: var(--surface);
}
.dot { width: 11px; height: 11px; border-radius: 50%; background: #333; }
.term-bar .dot:nth-child(1) { background: #ff5f56; }
.term-bar .dot:nth-child(2) { background: #ffbd2e; }
.term-bar .dot:nth-child(3) { background: #27c93f; }
.term-title { font-family: var(--vp-font-family-mono); font-size: 12px; color: var(--ink-2); margin-left: 6px; }
.term-state { margin-left: auto; font-family: var(--vp-font-family-mono); font-size: 11px; color: var(--hot); letter-spacing: .1em; }
.term-body {
  margin: 0;
  padding: 18px 18px 20px;
  font-family: var(--vp-font-family-mono);
  font-size: 12.5px;
  line-height: 1.75;
  color: var(--ink);
  white-space: pre;
  overflow-x: auto;
}
.term-body .t { color: var(--ink-2); float: right; }
.term-body .ok { color: #27c93f; }
.term-body .run { color: var(--hot); }
.term-body .c-prompt { color: var(--hot); }
.term-body .c-str { color: #ffd08a; }

/* ----------------------------- MARQUEE ----------------------------- */
.marquee {
  border-top: 1px solid var(--line);
  border-bottom: 1px solid var(--line);
  background: var(--surface);
  overflow: hidden;
  margin: 24px 0 0;
  white-space: nowrap;
}
.marquee-track {
  display: inline-flex;
  align-items: center;
  padding: 14px 0;
  animation: marquee 38s linear infinite;
}
.marquee-item {
  font-family: var(--animus-font-display);
  font-size: 14px;
  letter-spacing: .04em;
  color: var(--ink);
  padding: 0 4px;
  display: inline-flex;
  align-items: center;
}
.marquee-item .star { color: var(--hot); margin: 0 26px; }
@keyframes marquee { from { transform: translateX(0); } to { transform: translateX(-50%); } }

/* ----------------------------- BLOCKS ----------------------------- */
.block { padding: clamp(56px, 8vw, 104px) 24px; }
.block-grid {
  display: grid;
  grid-template-columns: 1fr 1.1fr;
  gap: 56px;
  align-items: center;
}
.block-head .display { margin: 16px 0 18px; }

/* comparison panel */
.panel { border: 1px solid var(--line); border-radius: 10px; overflow: hidden; background: var(--surface); }
.cmp-row {
  display: grid;
  grid-template-columns: 1.4fr repeat(4, 1fr);
  border-bottom: 1px solid var(--line);
}
.cmp-row:last-child { border-bottom: 0; }
.cmp-head { background: var(--vp-c-bg-alt, #000); }
.cmp-label, .cmp-cell {
  padding: 14px 12px;
  font-family: var(--vp-font-family-mono);
  font-size: 11px;
  letter-spacing: .05em;
  border-right: 1px solid var(--line);
  display: flex;
  align-items: center;
}
.cmp-cell { justify-content: center; }
.cmp-cell:last-child, .cmp-label:last-child { border-right: 0; }
.cmp-head .cmp-cell, .cmp-head .cmp-label { color: var(--ink-2); }
.cmp-cell.hot { background: rgba(255,122,0,.06); color: var(--hot); font-weight: 700; }
.cmp-label { color: var(--ink); }
.yes { color: var(--hot); font-weight: 700; }
.no { color: #555; }

/* model cards */
.model-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 12px; }
.model-card {
  border: 1px solid var(--line);
  border-radius: 8px;
  padding: 16px;
  background: var(--surface);
  transition: border-color .2s ease, transform .2s ease;
}
.model-card:hover { transform: translateY(-2px); border-color: #2e2e2e; }
.model-card.active { border-color: var(--hot); background: rgba(255,122,0,.05); }
.model-name { font-family: var(--animus-font-display); font-size: 15px; }
.model-tag { font-family: var(--vp-font-family-mono); font-size: 11px; color: var(--ink-2); margin: 4px 0 12px; }
.model-state { font-family: var(--vp-font-family-mono); font-size: 11px; color: var(--ink-2); display: flex; align-items: center; gap: 6px; }
.model-card.active .model-state { color: var(--hot); }

/* steps */
.steps { display: grid; grid-template-columns: repeat(4, 1fr); gap: 14px; margin-top: 40px; }
.step {
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 22px;
  background: var(--surface);
  text-decoration: none;
  color: var(--ink);
  transition: transform .2s ease, border-color .2s ease, box-shadow .2s ease;
  display: flex;
  flex-direction: column;
}
.step:hover { transform: translateY(-3px); border-color: var(--hot); box-shadow: 0 8px 30px rgba(255,122,0,.08); }
.step-n { font-family: var(--animus-font-display); font-size: 30px; color: var(--hot); line-height: 1; }
.step-title { font-family: var(--animus-font-display); font-size: 15px; margin: 14px 0 8px; }
.step-body { color: var(--ink-2); font-size: 13.5px; line-height: 1.55; flex: 1; }
.step-link { color: var(--hot); font-size: 12px; font-family: var(--vp-font-family-mono); margin-top: 16px; }

/* surface grid */
.surface-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 14px; margin-top: 40px; }
.surface-card {
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 22px;
  background: var(--surface);
  text-decoration: none;
  color: var(--ink);
  position: relative;
  transition: transform .2s ease, border-color .2s ease, box-shadow .2s ease;
}
.surface-card:hover { transform: translateY(-3px); border-color: var(--hot); box-shadow: 0 8px 30px rgba(255,122,0,.08); }
.surface-top { display: flex; justify-content: space-between; align-items: center; }
.surface-part { font-family: var(--vp-font-family-mono); font-size: 10.5px; letter-spacing: .08em; color: var(--ink-2); }
.surface-meta { font-family: var(--vp-font-family-mono); font-size: 10.5px; color: var(--hot); }
.surface-title { font-family: var(--animus-font-display); font-size: 19px; margin: 16px 0 10px; }
.surface-body { color: var(--ink-2); font-size: 13.5px; line-height: 1.55; }
.surface-arrow { color: var(--hot); margin-top: 16px; font-size: 18px; }

/* install */
.install { text-align: center; }
.install .display { margin: 16px 0 16px; }
.install-box {
  max-width: 760px;
  margin: 32px auto 22px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--vp-c-bg-alt, #000);
  overflow: hidden;
  text-align: left;
}
.install-bar { display: flex; align-items: center; gap: 7px; padding: 10px 14px; border-bottom: 1px solid var(--line); background: var(--surface); }
.install-label { font-family: var(--vp-font-family-mono); font-size: 12px; color: var(--ink-2); margin-left: 6px; }
.install-cmd { margin: 0; padding: 18px; font-family: var(--vp-font-family-mono); font-size: 13px; color: var(--ink); white-space: pre-wrap; word-break: break-all; }
.install-cmd .c-prompt { color: var(--hot); margin-right: 8px; }
.install-links { display: flex; flex-wrap: wrap; gap: 22px; justify-content: center; margin-top: 8px; }
.install-links a { font-family: var(--vp-font-family-mono); font-size: 13px; color: var(--ink-2); text-decoration: none; transition: color .2s ease; }
.install-links a:hover { color: var(--hot); }

/* ----------------------------- RESPONSIVE ----------------------------- */
@media (max-width: 900px) {
  .hero-grid, .block-grid { grid-template-columns: 1fr; gap: 36px; }
  .steps, .surface-grid { grid-template-columns: 1fr 1fr; }
  .model-grid { grid-template-columns: 1fr 1fr; }
}
@media (max-width: 560px) {
  .steps, .surface-grid, .model-grid { grid-template-columns: 1fr; }
  .cta-row .btn { flex: 1; text-align: center; }
}
</style>
