# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Bug Fixes

- Fix second exit code check missed by previous commit (cf3a593)
- Accept exit codes 4 (partial) and 5 (error) in e2e runner (29cca87)
- Use Agent Card name for A2A tool naming in context mode (bcddbcd)
- Add context-mode target aliases for A2A and AG-UI indicators (3185d6f)
- Redact API key in Debug output, remove .expect() from provider constructors (53c8713)
- Suppress raw JSON events when progress renderer is active (796c046)
- Gemini 3.1 thought_signature, indicator false positives, context-mode tool refresh (8bffba8)
- A2A skill lookup fallback and dynamic skill dispatch (d56573e)
- Enable sampling and reject elicitation in context mode (3a19f22)
- A2A tool names and temporal phase transitions in context mode (35f6998)
- Use max_completion_tokens for newer OpenAI models (886c5d5)
- Handle closed channels and server request errors in drive loop (a190619)
- Address review findings for context-mode (ed5ac83)
- Use valid API key pattern in OATF-006 credential exfil test (9b7d8e9)
- Move scenarios submodule up one level to eliminate double-nested path (41fcf0c)
- Default --progress auto to detailed on TTY (30a9ca7)
- Emit PhaseEntered only on actual phase changes (79b2750)
- Group messages by actor, fix protocol display and phase chain (da997b9)
- Remove surface-as-method filter and narrow scenario scan (33657a0)
- Capture per-request response channel for slow_stream delivery (e7e2321)
- Skip outgoing events in trigger evaluation and auto-increment server ports (51daff9)
- Release hardening — remove local-only validation, fix panics, update specs (442d9cb)
- Complete indicator trace filtering and add depth/actor validation (e3b6627)
- Include MCP-Protocol-Version header in HTTP client requests (944ed11)
- Use method-specific empty fallback shapes in dispatch_response (46abaca)
- Use taskId instead of id in MCP Tasks API handlers (89fec6b)
- Restore conformance.yaml and fix formatting (38e7f6d)
- Bound all attacker-controlled buffers (P1/P2/P3) (8b562e9)
- Allow cognitive_complexity on send_server_request (779c36b)
- Maintainability improvements across engine, tests, and CLI (5858d2d)
- Align run config parsing and quiet exits (d759637)
- Fix clippy/fmt and revert over-aggressive actor failure reporting (83fd01e)
- Harden v0.5 engine against review findings (72076fa)
- CLI behaviour alignment and test coverage ([#37](https://github.com/thoughtgate/thoughtjack/pull/37)) (bf5a425)
- Disable value_relayed indicator due to MCP server panic in 4-actor scenario (0937ae3)
- Fix extractor race, revert a2a min_traces, disable mcp-side-effects (4e0cf38)
- Pass node-version 22 to MCP conformance action (f54179c)
- Replace stdio with NullTransport in tests to prevent hangs (d1a72f5)
- Make --config optional for scenarios run (edd9401)
- Replace panicking signal handlers with graceful fallbacks (0e84fc8)
- Exclude fuzz crate from workspace to fix release builds (4473e94)
- URI template multi-byte panic + fuzz job permissions (6fd4b11)

### Build

- Update all Rust and GitHub Actions dependencies (9b04662)
- Bump GitHub Actions and Rust dependencies (70ecf08)

### CI/CD

- Add dns-rebinding-protection to conformance baseline (a719e03)
- Fetch scenarios submodule in checkout steps (a208ace)
- Add GitHub Pages deployment workflow and live status badges (697abfd)

### Documentation

- Add adversarial testing explanation and tutorial progress component (1be0601)
- Redesign docs site with indigo palette, styled components, and scenario cards (aec72e0)
- Fix P0 adoption blockers and update how-to guides to OATF format (e11e246)
- Align docs site with v0.5 OATF engine and progress output (72cff99)
- Fix remaining docs site issues and SEO optimization (6e2c14b)
- Fix stale scenario counts and deprecated trigger syntax (7c791a4)
- Fix stale file paths, scenario count, and Quick Start in README (908d932)
- Define OATF, add client mode tutorial, fix stale references (962d675)
- Fix exit codes, CLI flags, and trigger examples (e6c27de)

### Features

- AG-UI state extraction and isolated sampling context (33ac33a)
- Integrate OATF 0.4.0 output tiers (0d74a34)
- Tool disambiguation, event drain, AG-UI context extraction (b894b64)
- A2A context-mode overhaul — agent-level tools, event aliasing, indicator fix (cef42aa)
- Implement context-mode (TJ-SPEC-022) (c987222)
- Add Docker lab support and update CLAUDE.md (fd372db)
- Implement --export-trace for full protocol trace output (4a604c7)
- Add --progress compact|detailed flag with enriched events (5f6301f)
- Add interactive progress output for TTY sessions (c05dd61)
- Replace built-in scenarios with OATF official library (6d36225)
- Add unified /mcp endpoint for MCP Streamable HTTP transport (f1d01e9)
- Harden conformance suite with deeper assertions and attack coverage (05c369e)
- Add e2e conformance test infrastructure ([#35](https://github.com/thoughtgate/thoughtjack/pull/35)) (97acc74)

### Refactoring

- Split context.rs God Module, extract shared retry and A2A helpers (1de6c49)
- Consolidate A2A skill lookup into shared helpers (e78124a)
- Simplify context-mode drive loop (ca093e6)
- Simplify --progress to on/off/auto and default JSONL to stdout (a6cf8a3)
- Upgrade oatf SDK from 0.2 to 0.3 (1759136)
- Deduplicate local validation, merge A2A response resolution, bound MCP client reads (be0ba30)
- Archive v0.2 scenarios, migrate rug-pull to OATF (b5f6d11)

### Testing

- Add tier 1+2 coverage tests (f3c6732)
- Add high-priority coverage for indicator evaluation and context-mode driver (73e5478)
- Complete edge case coverage for context-mode (f86ed5c)
- Complete edge case coverage for context-mode (96e022e)
- Add edge case tests and verdict attribution for context-mode (a42c779)
- Add 44 tests for ProgressRenderer (30% → ~85% coverage) (882c328)
- Fix 6 test-quality issues from security review (T1–T6) (53419d5)

## [0.5.0] - 2026-03-02

### Added

- OATF-based execution engine with PhaseEngine, PhaseLoop, and PhaseDriver trait (TJ-SPEC-013)
- Multi-actor orchestration with ExtractorStore, ActorRunner, and Orchestrator (TJ-SPEC-015)
- Shared extractor publication via watch channels with cross-actor await support
- Verdict evaluation pipeline with grace period, CEL-based indicator evaluation, and JSON/human output (TJ-SPEC-014)
- MCP server PhaseDriver reimplemented on the v0.5 engine (TJ-SPEC-013 §8.2)
- MCP client PhaseDriver with split transport and server request handler (TJ-SPEC-018)
- A2A server PhaseDriver with Agent Card, task dispatch, and SSE streaming (TJ-SPEC-017)
- A2A client PhaseDriver with task message construction and streaming (TJ-SPEC-017)
- AG-UI client PhaseDriver with SSE streaming and message accumulation (TJ-SPEC-016)
- Shared SSE parser (`transport::sse`) with buffer overflow protection (16 MiB buffer, 4 MiB data limits)
- SharedTrace capacity limit (100,000 entries) to prevent unbounded memory growth
- HTTP request timeouts on SSE connection establishment
- Add continuous fuzzing infrastructure (b063c37)
- Auto-generate scenario index and fix MDX frontmatter (74072bd)
- Google Analytics tracking (da58aec)
- Auto-generate scenario index page and fix stale references (094b53d)

### Fixed

- Replace `unreachable!()` with proper error returns in retry loops
- Prevent information disclosure in A2A server JSON parse error responses
- SSE parser buffer limits prevent OOM from malicious servers
- Clean up `#[allow(dead_code)]` annotations with explanatory comments

### Not Included

- Semantic evaluation (LLM-as-judge via SemanticEvaluator) — returns `Skipped` verdict; planned for future release
- Synthesize generation (GenerationProvider) — returns graceful error; planned for future release

### Bug Fixes

- Skip release signing when triggered by PR runs (e33f0fc)
- Use static permissions values in workflow files (92d1177)
- Address Copilot security review feedback (e1509d2)
- Allow CDLA-Permissive-2.0 license in cargo-deny (72b7cb3)
- Set trailingSlash: false and convert relative links to absolute (7f31335)
- Patch lodash-es prototype pollution vulnerability (06963ad)
- Address Copilot review feedback on security workflow (409bf56)
- Make lychee link checker advisory (0209301)
- Add const to test helpers and fix needless_collect (8450849)
- Update lychee-action to valid v2.7.0 SHA (dd57bc3)
- Resolve clippy warnings in test files (be61f8a)
- Use correct CodeQL action SHA for v4.32.2 (63dcd6f)

### Build

- Update Cargo.lock to latest compatible versions (4e7136d)
- Bump GitHub Actions dependencies (a94e123)
- Bump reqwest from 0.12.28 to 0.13.2 and rand from 0.9.2 to 0.10.0 (b977232)
- Bump actions/upload-artifact from 4.6.2 to 6.0.0 ([#5](https://github.com/thoughtgate/thoughtjack/pull/5)) (8394c46)

### CI/CD

- Add Codecov token and slug to coverage upload (9397454)
- Pin downloadThenRun dependencies by hash (2b6123c)
- Move write permissions from top-level to job-level (67fec9f)
- Pin dtolnay/rust-toolchain to SHA in CodeQL workflow (b20f64f)
- Add test coverage with Codecov and lint test code (6f926d0)
- Add broken link checker to docs workflow (408277f)
- Add CodeQL SAST workflow (c0af69f)

### Documentation

- Add security policy and contributing guidelines (931a79e)

### Testing

- Add 5 config validation edge case tests (22b371b)
- Add 2 phase transition content verification tests (090b734)
- Add 4 HTTP error path tests (59b934f)
- Add 4 E2E tests for dynamic response pipeline (7c9a27b)
- Add 5 server resilience tests for error recovery paths (ef262ef)
- Add 27 E2E tests and fix 2 docgen unit test failures (a6f5c90)
