# Security Policy

## Important Context

ThoughtJack is an **offensive security testing tool**. It intentionally creates malicious MCP servers that deliver adversarial payloads, execute temporal attacks, and test client resilience. Behaviors described in attack scenarios (rug pulls, prompt injection, DoS payloads) are **features, not vulnerabilities**.

This policy covers vulnerabilities in ThoughtJack's own code — not in the attack scenarios it simulates.

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| 0.x     | :white_check_mark: |

## Reporting a Vulnerability

**Please do not open public issues for security vulnerabilities.**

Use [GitHub Security Advisories](https://github.com/thoughtgate/thoughtjack/security/advisories/new) to report vulnerabilities privately. This ensures the report is only visible to maintainers until a fix is released.

### What to include

- Description of the vulnerability
- Steps to reproduce
- Impact assessment
- Suggested fix (if any)

### What qualifies

- Memory safety issues bypassing `unsafe_code = "forbid"`
- Path traversal in configuration loading (`$include`, `$file` directives)
- Unintended code execution outside of explicitly configured external handlers
- Resource exhaustion that bypasses configured generator limits
- Credential or token leakage in logs or metrics

### What does NOT qualify

- Attack behaviors working as designed (that's the point)
- Denial of service against the ThoughtJack server itself (it's a testing tool)
- Social engineering via crafted YAML configurations (users control their own configs)

## Disclosure Timeline

We follow a 90-day coordinated disclosure process:

1. **Day 0**: Vulnerability reported via Security Advisory
2. **Day 1-7**: Acknowledgment and initial triage
3. **Day 7-60**: Fix development and testing
4. **Day 60-90**: Release preparation and advisory draft
5. **Day 90**: Public disclosure with fix available

We may request an extension for complex issues. We will not request extensions beyond 120 days.
