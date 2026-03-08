# oai-runner

## Purpose
Client adapter for OpenAI-compatible streaming APIs.

## Responsibilities
- Build and execute API calls for chat/completion streaming.
- Normalize responses into protocol-consumable event formats.
- Provide configurable transport behavior for model providers.

## Key Interfaces
- Streaming client entrypoints used by provider/runner layers.

## Local Structure
- `Cargo.toml`: HTTP client and auth dependencies.
- `src/`: request builders, response stream parsing, error handling.

## Notes
Keep provider-specific assumptions constrained to this crate.
