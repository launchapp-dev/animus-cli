# MCP OAuth (interactive `authorization_code`)

Animus can handle the OAuth login for OAuth-protected MCP servers (GitHub,
Linear, Notion, …) itself and expose them to agents through a local,
auth-free stdio proxy. This is the **interactive** `authorization_code` flow.
It sits alongside the machine-to-machine OAuth broker
([`docs/reference/secrets.md`](secrets.md) and the `oauth:` block in
[`workflow-yaml.md`](workflow-yaml.md)); the M2M flows
(`client_credentials`, `refresh_token`, `manual_bearer`) are unchanged.

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
3. At workflow runtime, any MCP server configured with the
   `authorization_code` flow is **repointed at `animus-mcp-proxy`** in the
   agent's MCP config. The agent talks to the local proxy over stdio with no
   auth. The proxy injects the live bearer token upstream and refreshes it
   before expiry and on a `401`.

The OAuth protocol itself is driven by [`rmcp`](https://crates.io/crates/rmcp)
1.7's `AuthorizationManager` / `AuthorizationSession`. Animus does not
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
| `scopes` | no | Requested at authorization; discovery/`WWW-Authenticate` can supply them |
| `client_id` | no | Pre-registered client id; when omitted, Dynamic Client Registration (RFC 7591) is used |

The machine-to-machine credential pointers (`token_url`, `client_id_env`,
`client_secret_env`, `refresh_token_env`, `bearer_env`) must **not** be set on
an `authorization_code` server — discovery fills those endpoints in, and
validation rejects them if present.

## CLI

```bash
# Interactive login (opens a browser, captures the loopback redirect):
animus mcp auth github
animus mcp auth github --scopes repo,read:user
animus mcp auth my-server --url https://mcp.example.com/   # server not in config yet

# Which servers are authenticated, token expiry per principal:
animus mcp auth-status
animus mcp auth-status --server github
animus mcp auth-status --server my-server --url https://mcp.example.com/  # not-in-config, URL-bound token

# Delete stored tokens for a server:
animus mcp auth-logout github
animus mcp auth-logout my-server --url https://mcp.example.com/           # not-in-config, URL-bound token
```

Tokens are bound to the upstream URL (see "Token storage" below), so for a
server you authenticated with `--url` (one not defined in config), pass the
same `--url` to `auth-status`/`auth-logout` to address its token.

All three honor the global `--json` flag for the `animus.cli.v1` envelope.

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

## Security

- The redirect-callback listener binds **loopback only** (`127.0.0.1:<ephemeral>`).
- The `state` (CSRF) parameter is validated against the value issued at
  authorization-URL generation; a mismatch is rejected before the code is used.
- The callback times out (5 minutes) so an abandoned login can't hang the CLI.
- Authorization codes, `state`, access/refresh tokens, client secrets, and the
  full authorization URL's code/state are never written to logs.

## The proxy binary

`animus-mcp-proxy` is a second binary in the `orchestrator-cli` package, so it
is built and installed by the standard `cargo build -p orchestrator-cli`
path right next to `animus`. It is normally launched by the runtime-contract
assembler, not by hand, but can be run directly:

```bash
animus-mcp-proxy --server github [--url https://api.githubcopilot.com/mcp/] [--project-root .]
```

It reads the live token from the keychain (written by `animus mcp auth`),
serves the agent an auth-free stdio MCP endpoint, and forwards to the upstream
with the bearer injected and refreshed on expiry/`401`.

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
