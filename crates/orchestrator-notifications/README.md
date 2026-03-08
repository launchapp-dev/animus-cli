# orchestrator-notifications

## Purpose
Implements notification abstractions and delivery mechanisms for AO events.

## Responsibilities
- Define notification types and targets.
- Deliver structured messages for workflows and runtime events.
- Encapsulate optional transport integrations behind composable providers.

## Key Interfaces
- Notification services used by operational workflows.

## Local Structure
- `Cargo.toml`: dependencies for formatting and transport.
- `src/`: providers, message models, and sender implementations.

## Notes
Keep notification payloads domain-oriented so upstream crates remain loosely coupled.
