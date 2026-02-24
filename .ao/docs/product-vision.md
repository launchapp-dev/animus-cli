# Product Vision

## Project
- Name: ao-cli

## Problem
Manual orchestration of AI agents and CLI workflows is fragmented and unsafe, inconsistent, and hard to observe.

## Target Users
- developers and automation operators using AO

## Goals
- Provide a deterministic, local-first Rust AO CLI that unifies project planning, execution, review/QA, and audit trails under one control plane
- Increase operator confidence through explicit safety gates, reproducible run artifacts, and auditable state transitions
- Make AO fully machine-operable via structured JSON outputs for automation and policy enforcement

## Constraints
- Keep the project Rust-only with no desktop-wrapper (Tauri) dependency
- Do not replace existing AO state files with non-machine-readable outputs
- Deliver command safety through confirmations for destructive operations and controlled destructive flows
- Ensure all runtime artifacts and history are traceable through `.ao/` state, run events, and task outputs

## Value Proposition
AO provides a deterministic, inspectable, machine-friendly CLI control plane for orchestrating agent work from vision and requirements through execution and QA with auditable outcomes.

## Complexity
- Tier: medium
- Confidence: 0.60
- Recommended requirement range: 8-14
- Task density: medium
- Rationale: Complexity inferred from vision scope, constraints, and delivery expectations.
