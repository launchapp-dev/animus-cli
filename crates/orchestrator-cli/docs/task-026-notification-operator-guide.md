# TASK-026 Operator Guide: Daemon Notification Connectors

## Overview
Daemon notification connectors route selected daemon events to external endpoints
without storing raw secrets in repo-local state.

Implemented connector types:
- `webhook`: generic HTTP JSON POST.
- `slack_webhook`: Slack-compatible webhook POST payload.

## Configuration Contract
Notification config schema:
- `schema`: `ao.daemon-notification-config.v1`
- `version`: `1`
- `connectors`: connector definitions (`id`, `type`, `enabled`, env-var refs)
- `subscriptions`: event routing filters
- `retry_policy`: `max_attempts`, `base_delay_secs`, `max_delay_secs`
- `max_deliveries_per_tick`: bounded async flush budget

### Safe Example (`notification-config.json`)
```json
{
  "schema": "ao.daemon-notification-config.v1",
  "version": 1,
  "connectors": [
    {
      "type": "webhook",
      "id": "ops-webhook",
      "enabled": true,
      "url_env": "AO_NOTIFY_WEBHOOK_URL",
      "headers_env": {
        "Authorization": "AO_NOTIFY_WEBHOOK_BEARER"
      },
      "timeout_secs": 10
    },
    {
      "type": "slack_webhook",
      "id": "ops-slack",
      "enabled": true,
      "webhook_url_env": "AO_NOTIFY_SLACK_WEBHOOK_URL",
      "timeout_secs": 10,
      "username": "AO Daemon"
    }
  ],
  "subscriptions": [
    {
      "id": "workflow-phase-failures",
      "enabled": true,
      "connector_id": "ops-webhook",
      "event_types": [
        "workflow-phase-failed",
        "workflow-phase-contract-violation"
      ]
    },
    {
      "id": "all-logs-for-project",
      "enabled": true,
      "connector_id": "ops-slack",
      "event_types": ["log"],
      "project_root": "/absolute/project/root"
    }
  ],
  "retry_policy": {
    "max_attempts": 5,
    "base_delay_secs": 2,
    "max_delay_secs": 300
  },
  "max_deliveries_per_tick": 8
}
```

## Applying Config
Apply via file:
```bash
ao daemon config --notification-config-file ./notification-config.json
```

Apply via inline JSON:
```bash
ao daemon config --notification-config-json '{"schema":"ao.daemon-notification-config.v1","version":1,"connectors":[],"subscriptions":[],"retry_policy":{"max_attempts":5,"base_delay_secs":2,"max_delay_secs":300},"max_deliveries_per_tick":8}'
```

Clear notification config:
```bash
ao daemon config --clear-notification-config
```

Inspect current daemon config:
```bash
ao daemon config
```

## Credential Handling and Redaction
- Store only env-var names in daemon config (`*_env` fields).
- Never store raw webhook URLs/tokens in `.ao/pm-config.json`.
- Missing credentials become redacted delivery errors and are dead-lettered.
- Runtime lifecycle events include redacted error metadata only.

## Subscription Filters
Each subscription can filter on:
- `event_types`: exact (`"health"`) or wildcard (`"workflow-phase-*"`, `"*"`)
- `project_root`: optional exact match
- `workflow_id`: optional exact match (from daemon event payload)
- `task_id`: optional exact match (from daemon event payload)

## Retry and Dead-Letter Behavior
- Transient failures (`HTTP 5xx`, `429`, timeouts, connect failures) retry with exponential backoff.
- Permanent failures (`HTTP 4xx` except `429`) dead-letter immediately.
- Exhausted retries dead-letter after `max_attempts`.
- Delivery lifecycle events emitted:
  - `notification-delivery-enqueued`
  - `notification-delivery-sent`
  - `notification-delivery-failed`
  - `notification-delivery-dead-lettered`

Persistent runtime files are under repo-scoped AO state:
- `~/.ao/<repo-scope>/notifications/outbox.jsonl`
- `~/.ao/<repo-scope>/notifications/dead-letter.jsonl`

## Troubleshooting
- `missing credential env var ...`: set referenced env vars in daemon runtime environment.
- Frequent `notification-delivery-failed`: inspect endpoint availability, status codes, and retry policy.
- Repeated dead-letter entries: validate connector id, env refs, and endpoint auth.
- Daemon scheduling continues even if notification delivery fails; monitor lifecycle events with:
```bash
ao daemon events --follow true
```
