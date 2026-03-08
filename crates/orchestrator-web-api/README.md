# orchestrator-web-api

## Purpose
Implements AO web API business logic and service layer.

## Responsibilities
- Expose business operations for AO entities and workflows.
- Map domain actions into API-facing service calls.
- Encapsulate orchestration-specific use cases for web delivery.

## Key Interfaces
- Service modules used by HTTP handlers in the web server crate.
- API DTO conversions and validation logic.

## Local Structure
- `Cargo.toml`: service dependencies and contract usage.
- `src/`: service handlers, adapters, and orchestration glue.

## Notes
This crate stays focused on business logic and intentionally stays thin in transport concerns.
