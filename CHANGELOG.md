# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

### Features

- Add Google Analytics tracking (da58aec)
- Auto-generate scenario index and fix MDX frontmatter (74072bd)
- Add continuous fuzzing infrastructure (b063c37)
- Auto-generate scenario index page and fix stale references (094b53d)

### Testing

- Add 5 config validation edge case tests (22b371b)
- Add 2 phase transition content verification tests (090b734)
- Add 4 HTTP error path tests (59b934f)
- Add 4 E2E tests for dynamic response pipeline (7c9a27b)
- Add 5 server resilience tests for error recovery paths (ef262ef)
- Add 27 E2E tests and fix 2 docgen unit test failures (a6f5c90)
