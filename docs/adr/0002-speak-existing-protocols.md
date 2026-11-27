# ADR 0002 — Ingest OTLP and Ray-compatible payloads, not only our own

Status: accepted · 2026-11-27

## Context
Most programs already emit something: OpenTelemetry spans and logs, Ray `ray()` calls, plain log
lines. LLM frameworks increasingly emit OpenTelemetry `gen_ai.*` spans (OpenLLMetry, Vercel AI SDK,
vendor instrumentors).

## Decision
Ship ingest adapters for OTLP/HTTP (`:4318`), the Ray HTTP protocol (`:23517`), stdin/NDJSON
(`glint pipe`), log-file tailing and agent hooks, all translating to the native Envelope. Each
adapter has a documented mapping table and golden tests.

## Consequences
- Day-one coverage for any language with an OTel SDK, and for existing Ray client libraries.
- LLM tracing works with zero Glint-specific code via `gen_ai.*` → `llm.call`.
- We must track upstream semconv changes; mapping tables make that explicit.
- Port conflicts are possible (a real OTel collector on 4318): adapters disable themselves and
  report it in `/v1/info` instead of failing the daemon.
- Ray compatibility covers the open-source client wire format only; Glint does not copy Ray's app,
  name or branding.
