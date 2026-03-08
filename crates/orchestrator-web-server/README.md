# orchestrator-web-server

## Purpose
Runs the Axum web server and serves AO web traffic and embedded frontend assets.

## Responsibilities
- Configure and start HTTP server runtime.
- Wire routes, middleware, and request context.
- Serve API endpoints and static content.
- Coordinate with web API/business services for responses.

## Key Interfaces
- Route setup and server bootstrap entrypoints.
- Runtime wiring for web execution mode and health checks.

## Local Structure
- `Cargo.toml`: axum and HTTP ecosystem dependencies.
- `src/`: server bootstrap, routing, and app composition.

## Notes
This crate is the transport boundary; business logic should live in downstream service crates.
