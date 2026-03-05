# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Bug Fixes

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

- Bump GitHub Actions and Rust dependencies (70ecf08)

### CI/CD

- Add GitHub Pages deployment workflow and live status badges (697abfd)

### Documentation

- Fix stale scenario counts and deprecated trigger syntax (7c791a4)
- Fix stale file paths, scenario count, and Quick Start in README (908d932)
- Define OATF, add client mode tutorial, fix stale references (962d675)
- Fix exit codes, CLI flags, and trigger examples (e6c27de)

### Features

- Harden conformance suite with deeper assertions and attack coverage (05c369e)
- Add e2e conformance test infrastructure ([#35](https://github.com/thoughtgate/thoughtjack/pull/35)) (97acc74)

### Refactoring

- Archive v0.2 scenarios, migrate rug-pull to OATF (b5f6d11)

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
