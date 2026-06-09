# MoonlightCode — house rules for implementing agents

Single-build pure-Rust desktop IDE (GPUI) for orchestrating Claude Code sessions.
Full plan lives in `.bmad-output/planning/` (brief, prd, architecture, ux-design-specification) and `.bmad-output/spike-0-findings.md`. The architecture is the source of truth.

## Hard rules (from architecture.md)
- **Hexagonal, domain-first.** `crates/domain` is PURE: entities + trait ports + `thiserror` errors only. **No tokio, no I/O, no GPUI** in `domain` (CI-enforced).
- **Ports & adapters.** Engine/UI depend on `domain` traits; adapter crates implement them. UI/engine never call an adapter directly — always through a port.
- **One composition root:** `apps/desktop` is the ONLY place concrete adapters are wired into ports (`Arc<dyn Trait>` constructor injection). No global singletons, no service locator.
- **UI ↔ engine boundary:** UI reads engine state via `watch` read-models and writes via `Command`s. UI never mutates engine state directly.
- **One permission authority:** every gated/autonomous action goes through the `PolicyDecisionPoint`, then the append-only audit log. No permission check lives anywhere else.
- **Enums over booleans** for domain states (`Phase`, `SessionStatus`, `TrustTier`). Never stringly-typed.
- **Errors:** typed `thiserror` domain enums across port boundaries; `anyhow` allowed only inside `apps/desktop` glue and adapter internals.
- **No `unwrap()`/`expect()`** in non-test code except provably-infallible with a `// SAFETY:` comment. **No `println!`** — use `tracing`.
- **No hardcoded colors in UI** — read design tokens (`apps/desktop/src/views/theme.rs`). Status is always badge + color + border (never color-only).

## Conventions
- Crates: kebab-case dir, snake_case package (`mcp-server` → `moonlight-mcp-server`). Modules: snake_case. Types: PascalCase. Traits = ports, role-named (`ControlPort`, `SessionStore`).
- DB: SQLite, snake_case plural tables, ULID `id`, epoch-millis timestamps, forward-only timestamped migrations. Audit log append-only (revert = compensating entry).
- Tests: unit tests in-file (`#[cfg(test)] mod tests`); integration tests in each crate's `tests/`.

## Verify before claiming done
- `cargo fmt --all` · `cargo clippy --all-targets -- -D warnings` · `cargo test` · `cargo run -p moonlight` (binary name `moonlight`).
- Domain purity: `domain` must not depend on tokio/IO/GPUI.
