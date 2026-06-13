# MCP OAuth (`authorization_code` + broker-backed M2M flows)

Animus can handle the OAuth login for OAuth-protected MCP servers (GitHub,
Linear, Notion, …) itself and expose them to agents through a local,
auth-free stdio proxy. For the interactive case this is the
`authorization_code` flow. For machine-to-machine servers
(`client_credentials`, `refresh_token`, `manual_bearer`), the same proxy now
resolves the live bearer through the OAuth broker at connect time instead of
embedding a resolved `Authorization` header into the agent contract.

## How it works

```text
  agent ── stdio (no auth) ──▶ animus-mcp-proxy ── streamable-http + Bearer ──▶ upstream MCP
```

1. You run `animus mcp auth <server>` once. Animus discovers the server's
   authorization server, performs Dynamic Client Registration (or uses a
   pre-registered `client_id`), runs the OAuth 2.1 authorization-code + PKCE
   flow, opens your browser, captures the redirect on a loopback port, and
   exchanges the code for tokens.
2. Tokens are stored in the **OS keychain** (the same store as
   `animus secret`), keyed per server + principal.
3. At runtime, any MCP server configured with an `oauth:` block is
   **repointed at `animus-mcp-proxy`** in the agent's MCP config. The agent
   talks to the local proxy over stdio with no auth. The proxy injects the
   live bearer token upstream and refreshes or re-resolves it on expiry and
   on a `401`.

The interactive OAuth protocol is driven by
[`rmcp`](https://crates.io/crates/rmcp) 1.7's `AuthorizationManager` /
`AuthorizationSession`. For broker-backed M2M flows, `animus-mcp-proxy`
delegates bearer resolution to the Animus OAuth broker. Animus does not
hand-roll OAuth, PKCE, or token exchange.

## Configuration

Add an `oauth` block with `flow: authorization_code` to an HTTP-transport MCP
server in workflow YAML (`.animus/workflows.yaml` /
`.animus/workflows/*.yaml`) or in project config (`.animus/config.json`,
`mcp_servers`):

```yaml
mcp_servers:
  github:
    transport: http
    url: https://api.githubcopilot.com/mcp/
    oauth:
      flow: authorization_code
      # optional — discovery + DCR fill these in when omitted:
      scopes:
        - repo
        - read:user
      client_id: my-pre-registered-client      # skip Dynamic Client Registration
```

Fields:

| Field | Required | Notes |
|---|---|---|
| `flow` | yes | `authorization_code` |
| `url` | yes | Upstream HTTP MCP endpoint; also the OAuth `resource` indicator (RFC 8707), the discovery seed (RFC 9728 protected-resource metadata → authorization server), and the proxy target |
| `scopes` | no | Requested at authorization. **When omitted, Animus auto-detects the server's advertised `scopes_supported`** from discovery metadata and requests those (see "Scope posture" below); if the server advertises none, nothing is requested and it applies its own minimal default. Set this to pin a specific (narrower) scope set, or `[none]` to force no scopes (opt out of auto-detection). |
| `client_id` | no | Pre-registered client id; when omitted, Dynamic Client Registration (RFC 7591) is used |

The machine-to-machine credential pointers (`token_url`, `client_id_env`,
`client_secret_env`, `refresh_token_env`, `bearer_env`) must **not** be set on
an `authorization_code` server — discovery fills those endpoints in, and
validation rejects them if present.

## CLI

```bash
# Interactive login (previews the requested scopes, asks for y/N, then opens a
# browser and captures the loopback redirect):
animus mcp auth github
animus mcp auth github --scopes repo,read:user
animus mcp auth github --yes               # skip the consent prompt
animus mcp auth github --dry-run           # show resolved scopes; no browser, no token
animus mcp auth my-server --url https://mcp.example.com/   # server not in config yet

# Which servers are authenticated, token expiry per principal:
animus mcp auth-status
animus mcp auth-status --server github
animus mcp auth-status --server my-server --url https://mcp.example.com/  # not-in-config, URL-bound token

# Delete stored tokens for a server:
animus mcp auth-logout github
animus mcp auth-logout my-server --url https://mcp.example.com/           # not-in-config, URL-bound token
```

`animus mcp auth` flags:

| Flag | Effect |
|---|---|
| `--scopes a,b` | Comma-separated scopes to request; overrides config `scopes:` **and** the server's advertised scopes. When omitted, scopes resolve to config `scopes:`, else the server's advertised `scopes_supported` (auto-detected), else none. Pass this to narrow an over-broad auto-detected set, or `--scopes none` to force **no** scopes (opt out of auto-detection; server default applies). |
| `--url <u>` | Upstream MCP URL for a server not yet in config. |
| `--yes` | Skip the interactive consent prompt and open the browser immediately. |
| `--dry-run` | Run OAuth discovery (validates the endpoint + auth-server metadata), resolve the scope set, and report them (incl. whether DCR would run) **without** opening a browser, binding the callback, constructing the keychain token store, or obtaining any token. A broken/unreachable endpoint surfaces a discovery error. Exits 0. |
| `--json` | Emit the `animus.cli.v1` envelope. Implies non-interactive (skips the prompt) and includes the resolved `requested_scopes` plus a `scopes_auto_detected` flag. |

Tokens are bound to the upstream URL (see "Token storage" below), so for a
server you authenticated with `--url` (one not defined in config), pass the
same `--url` to `auth-status`/`auth-logout` to address its token.

All three honor the global `--json` flag for the `animus.cli.v1` envelope.

### Scope posture (auto-detect advertised scopes)

Scope resolution precedence (highest first):

1. **`--scopes` (CLI override)** — explicit, wins.
2. **config `scopes:`** — explicit.
3. **the server's advertised scopes** — auto-detected from the discovery
   metadata when neither explicit source is set. This adopts rmcp's
   highest-priority advertised set (SEP-835 order): the `WWW-Authenticate`
   `scope` from the server's initial `401` if present, else RFC 9728
   protected-resource-metadata `scopes_supported`, else RFC 8414
   authorization-server-metadata `scopes_supported` (plus `offline_access` when
   the server advertises it).
4. **none** — when the server advertises no scopes either, nothing is requested
   and the authorization server applies its own minimal default.

Tiers 1–2 are unchanged. Tier 3 (auto-detect) exists because some authorization
servers **require** a specific scope and **fail** the authorize step when an
empty scope is requested — e.g. Robinhood's trading MCP advertises
`"scopes_supported": ["internal"]` and rejects an empty-scope authorize. Reading
the advertised set off the discovery metadata Animus already fetches makes those
servers work out of the box, without the user having to discover and type the
scope by hand.

Auto-detected scopes are clearly **marked as such** everywhere they surface (the
consent preview, `--dry-run` output, and the `--json` envelope's
`scopes_auto_detected: true`) so you know that **all** advertised scopes were
adopted. If that set is broader than you want, narrow it with `--scopes` (or pin
config `scopes:`) — both override the advertised set and render as *not*
auto-detected.

**Opting out of auto-detection.** For a server that advertises a broad
*optional* scope set but still accepts a bare (no-`scope`) authorize, pass
`--scopes none` (or set config `scopes: [none]`), case-insensitive, to force an
empty request and keep the server's own minimal default — the pre-auto-detect
behavior. `none` is only the opt-out sentinel when it is the **sole** scope; a
literal scope actually named `none` alongside others is requested verbatim.

Scope resolution runs **after** discovery (the advertised set only exists
post-discovery), so the consent gate also runs after discovery in order to show
you the real resolved scopes before the browser opens. Discovery is a read-only
GET of public metadata and consults no credentials, so a consent "no" still
obtains no token and touches no keychain.

Before opening the browser, the flow prints a preview to stderr and asks for
confirmation (default **No**):

```text
animus mcp auth: about to authorize 'robinhood-trading' (https://agent.robinhood.com/mcp/trading)
  requesting scopes: internal (auto-detected from server metadata)
Open browser to authorize? [y/N]
```

When the server advertises no scopes and none are configured, the preview reads
`requesting scopes: (none — server default / minimal)` instead.

The preview never prints the authorization URL (it carries PKCE + `state`) —
only the server, base URL, and the exact scopes being requested. Pass `--yes`
(or `--json`) to skip the prompt in scripts; `--json` still emits the resolved
`requested_scopes` (and `scopes_auto_detected`) in the envelope so an automated
caller can audit breadth.

If the proxy has no stored token (or a refresh is rejected), it returns a
clear MCP error instructing you to run `animus mcp auth <server>`.

## Token storage

Tokens live in the OS keychain (macOS Keychain, libsecret on Linux, Windows
Credential Manager) under the project's keychain scope — the same backend as
`animus secret`. The logical key is `mcp-oauth:<server>:<principal>` **and is
bound to the upstream URL**; because keychain KEYs are restricted to
`[A-Za-z_][A-Za-z0-9_]*`, the stored entry is keyed
`MCP_OAUTH__<sanitized-server-principal>__<hash>` where the hash covers
`server`, `principal`, and `url`, with the rmcp `StoredCredentials` bundle
serialized as the value. Token values never touch disk outside the keychain and
are never logged.

Binding the token to the URL is a security control: if a server name is reused
but repointed at a different host (a workflow override, or an untrusted branch
swapping `github`'s URL), the derived key changes, so the bearer minted for the
original host is simply not found — resolution fails closed and forces a fresh
`animus mcp auth` rather than leaking the token to a host it was never issued
for.

The `<principal>` is the RBAC default principal (`local` in the single-user
default), so a future multi-user surface can hold distinct tokens per user
without a storage migration.

## Recovery

- **Corrupt keychain credentials never block their own repair.** If the
  stored `StoredCredentials` JSON is unparseable (truncated write, schema
  skew), it is treated as *no stored credentials*: the proxy reports the
  server as needing `animus mcp auth <server>`, `animus mcp auth-status`
  shows it unauthenticated, and `animus mcp auth-logout` still clears the
  entry. Re-running `animus mcp auth` overwrites it.
- **Broker-backed `refresh_token` flows self-heal a dead rotation chain.**
  The broker caches rotated refresh tokens per server under
  `~/.animus/<scope>/mcp-oauth-cache/`. If the server rejects the cached
  refresh token with an explicit RFC 6749 `invalid_grant`, the cache entry
  is deleted and the env-var seed is retried within the same resolution
  (other failures — `invalid_client`, 429/5xx, transport errors — keep the
  cached rotation chain intact). Each
  cache entry also records a hash of the seed env var's *value* (never the
  plaintext), so re-minting the seed (a new token in the same env var)
  invalidates the stale entry automatically; a cache populated while the
  seed was present stays usable in a process where the env var is absent.
- **Concurrent resolutions can't revoke a rotating grant family.** Cached
  broker flows serialize the expiry-check → refresh → cache-write cycle
  behind a per-server file lock and re-read the cache after acquiring it,
  so parallel phases perform one token POST instead of racing the same
  refresh token.

## Security

- The redirect-callback listener binds **loopback only** (`127.0.0.1:<ephemeral>`).
- The `state` (CSRF) parameter is validated against the value issued at
  authorization-URL generation; a mismatch is rejected before the code is used.
- The callback times out (5 minutes) so an abandoned login can't hang the CLI.
- **Scopes requested are explicit-then-advertised** (see "Scope posture"): with
  no `--scopes`/config scopes, the server's advertised `scopes_supported` is
  adopted (marked auto-detected) so required-scope servers work; if nothing is
  advertised, nothing is requested. Narrow an over-broad advertised set with
  `--scopes`.
- **Preview + confirm before the browser opens.** The exact (possibly
  auto-detected) scopes + server are shown and a `y/N` (default No) is required;
  `--dry-run` inspects the resolved request without authorizing. The consent
  gate runs **after** discovery (so the preview shows the real auto-detected
  scopes) but **before** any callback bind or browser open — discovery is a
  read-only public-metadata GET, so a "no" still obtains no token and touches no
  keychain.
- Authorization codes, `state`, access/refresh tokens, client secrets, and the
  full authorization URL's code/state are never written to logs — and the
  consent preview never prints the authorization URL either.

## The proxy binary

`animus-mcp-proxy` is a second binary in the `orchestrator-cli` package, so it
is built and installed by the standard `cargo build -p orchestrator-cli`
path right next to `animus`. It is normally launched by the runtime-contract
assembler, not by hand, but can be run directly:

```bash
animus-mcp-proxy --server github [--url https://api.githubcopilot.com/mcp/] [--project-root .]
```

It serves the agent an auth-free stdio MCP endpoint and forwards to the
upstream with a live bearer token:

- `authorization_code`: from the OS keychain entry written by
  `animus mcp auth`
- `manual_bearer`, `client_credentials`, `refresh_token`: from the OAuth
  broker at connect time

On upstream auth failure it retries once with a forced refresh/re-resolution.

## Limitations

- **Client-driven server features are not yet relayed.** The proxy forwards
  the agent's requests/notifications upstream and the upstream's responses
  back, and it injects + refreshes the bearer. It does **not** yet relay
  *server→client* traffic — `sampling/createMessage`, `roots/list`,
  `elicitation`, and server→client list-changed notifications are handled by
  an empty client inside the proxy rather than being forwarded to the agent.
  Tools-only OAuth servers (GitHub, Linear, Notion) work today; MCP servers
  that drive sampling/roots/elicitation through the client are not yet
  transparent behind the proxy.
- **`authorization_server` discovery override is not configurable.** rmcp 1.7
  ties the OAuth `resource` indicator (RFC 8707) and the discovery seed to a
  single base URL, so the MCP `url` is always used for both. Servers must
  expose protected-resource metadata (RFC 9728) from their MCP URL for
  discovery to resolve the authorization server.
