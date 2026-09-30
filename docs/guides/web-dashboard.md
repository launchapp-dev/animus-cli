# Web Dashboard Guide

The Animus web dashboard ships as a set of standalone plugins. The CLI no
longer bundles an in-process HTTP server. Instead, `animus web` discovers and
spawns installed `transport_backend` and `web_ui` plugins.

## Installing the Web Stack

Install the curated transport + UI set in one shot:

```bash
animus plugin install-defaults --include-transports
```

Or install them individually:

```bash
animus plugin install animus-ecosystem/animus-transport-http
animus plugin install animus-ecosystem/animus-transport-graphql
animus plugin install launchapp-dev/animus-web-ui
```

The exact curated tags live in
`crates/orchestrator-core/src/plugin_registry.rs`, so prefer
`animus plugin install-defaults --include-transports` when you want the
checked-in set instead of unpinned latest releases.

## Starting the Web UI

Spawn the installed transport plugins and report their bound URLs:

```bash
animus web serve
```

Open the resolved UI URL in a browser:

```bash
animus web open
```

`animus web serve --open` does both at once. If no transport plugins are
installed, the command exits non-zero and prints the install commands above.
With only `animus-transport-http` installed, `animus web` still starts and
falls back to the API URL; adding `animus-web-ui` (or another plugin that
advertises the `$ui/web` capability) upgrades that fallback to the browser UI.
If an installed transport or UI plugin declares required env vars in its
manifest, those vars must also be set before `animus web` will spawn it.
Use `animus plugin info <plugin-name>` to inspect `env_required`
when startup fails before a URL is reported.

## How the Pieces Connect

| Plugin | Default address | Serves |
|---|---|---|
| `animus-transport-http` | `127.0.0.1:8080` | REST API (`/api/v1`) |
| `animus-transport-graphql` | `127.0.0.1:8081` | GraphQL API, subscriptions at `/graphql/ws` |
| `animus-web-ui` | `127.0.0.1:8082` | The dashboard; proxies `/graphql` and `/graphql/ws` to the GraphQL transport |

`animus web serve` starts the API transports first and passes the GraphQL
transport's bound address to the web UI as `api_origin`, so the dashboard
reaches the API even when the GraphQL transport isn't on its default port.
Both transports talk to the daemon over its control socket, so start the
daemon (`animus daemon start`) for the dashboard to show data.

The GraphQL API has no login. The GraphQL transport and the web UI therefore
answer only requests addressed to this machine: the `Host` header must be
`localhost`, a `127.x.x.x` address or `[::1]`, and a browser `Origin`, when
sent, must be one of those too. Other websites open in the same browser get
`403`. To reach the dashboard through another host name on purpose, list it
in the plugin's `allowed_hosts` config.

## URL Override

`animus web open --url https://my-tunnel.example` skips plugin discovery and
opens the supplied URL directly. Use `--path /runs` to append a sub-path to
the resolved URL.

## Architecture

The web stack lives in three external repositories: the two API transports
under the [`animus-ecosystem`](https://github.com/animus-ecosystem) org and
the dashboard under [`launchapp-dev`](https://github.com/launchapp-dev):

| Repository | Role |
|-------|------|
| `animus-transport-http` | REST + SSE HTTP transport plugin |
| `animus-transport-graphql` | GraphQL transport plugin |
| `animus-web-ui` | React dashboard bundled by a wrapper plugin |

The CLI discovers them through the standard plugin host registry and plugin
search paths, then spawns them via `PluginHost::spawn_with_options` using the
plugin manifest's `env_required` contract. Plugins return their bound URL in
the `transport/start` reply (older plugins: the JSON-RPC `initialize`
handshake or the optional `transport/info` call). See
[`crates/orchestrator-cli/src/services/operations/ops_web.rs`](../../crates/orchestrator-cli/src/services/operations/ops_web.rs).
