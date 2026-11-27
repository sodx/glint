# ADR 0001 — Protocol first, thin clients

Status: accepted · 2026-11-27

## Context
Glint must eventually support every language and framework. Writing and maintaining rich SDKs for
30 ecosystems is not feasible for a small team, and SDK quality drift is what makes multi-language
tools feel broken.

## Decision
- The wire protocol (`docs/PROTOCOL.md`, `spec/*.schema.json`) is the product's primary interface.
- The envelope is generic (`kind` + open `payload`); the core validates envelopes, not payloads.
- SDKs are thin (build envelope, capture call site, async POST) and are "supported" only when they
  pass the language-neutral conformance suite in `spec/conformance/`.
- Framework support is layered on top of SDKs and emits existing kinds or `x-<framework>.*` kinds.

## Consequences
- Adding a language is days, not weeks, and the community can do it without touching Rust.
- Rich, kind-specific UX depends on known kinds; unknown kinds get a generic but usable view.
- Schema discipline is mandatory: kinds change additively only.
