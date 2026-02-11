# Security Policy

## Supported Versions

We provide security updates for the following versions:

| Version | Supported          |
| ------- | ------------------ |
| 0.4.x   | :white_check_mark: |
| < 0.4   | :x:                |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, report security vulnerabilities via GitHub Security Advisories:

1. Go to https://github.com/thoughtgate/thoughtjack/security/advisories
2. Click "Report a vulnerability"
3. Fill in the details of the vulnerability
4. Submit the report

You should receive a response within 48 hours. If the issue is confirmed, we will:

1. Develop a fix
2. Prepare a security advisory
3. Release a patched version
4. Publish the security advisory

## Security Features

### Release Artifact Signing

All release artifacts are signed using [Sigstore](https://www.sigstore.dev/) (keyless signing with GitHub OIDC tokens). This provides:

- **Cryptographic verification** of artifact authenticity
- **Transparency log** for public audit trail
- **No secret key management** (automated via GitHub Actions)

#### Verifying Release Signatures

1. Install cosign:
   ```bash
   # macOS
   brew install cosign

   # Linux
   wget https://github.com/sigstore/cosign/releases/latest/download/cosign-linux-amd64
   sudo mv cosign-linux-amd64 /usr/local/bin/cosign
   sudo chmod +x /usr/local/bin/cosign
   ```

2. Download a release artifact and its signature:
   ```bash
   gh release download v0.4.1 -p 'thoughtjack-*-linux.tar.gz*'
   ```

3. Verify the signature:
   ```bash
   cosign verify-blob \
     --certificate thoughtjack-*-linux.tar.gz.pem \
     --signature thoughtjack-*-linux.tar.gz.sig \
     --certificate-identity-regexp="^https://github.com/thoughtgate/thoughtjack" \
     --certificate-oidc-issuer="https://token.actions.githubusercontent.com" \
     thoughtjack-*-linux.tar.gz
   ```

4. Check the transparency log (optional):
   ```bash
   rekor-cli search --artifact thoughtjack-*-linux.tar.gz
   ```

### Continuous Fuzzing

ThoughtJack uses [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) (libFuzzer) to continuously test for security vulnerabilities. Fuzzing runs nightly for 4 hours across four high-value targets:

1. **Config Loader** - YAML parsing, directives, recursion limits
2. **JSON-RPC Parser** - Malformed messages, binary framing
3. **Phase Trigger** - Regex patterns, content matching edge cases
4. **Nested JSON Generator** - Depth limits, stack overflow prevention

#### Running Fuzzing Locally

1. Install the nightly toolchain and cargo-fuzz:
   ```bash
   rustup toolchain install nightly
   cargo install cargo-fuzz
   ```

2. Run a specific fuzz target:
   ```bash
   cd fuzz
   cargo +nightly fuzz run fuzz_config_loader
   ```

3. Run with time limit:
   ```bash
   cargo +nightly fuzz run fuzz_config_loader -- -max_total_time=300
   ```

4. View coverage:
   ```bash
   cargo +nightly fuzz coverage fuzz_config_loader
   ```

#### Reporting Fuzzing Crashes

If you discover a crash while fuzzing locally:

1. Minimize the crash input:
   ```bash
   cargo +nightly fuzz tmin fuzz_config_loader fuzz/artifacts/fuzz_config_loader/crash-*
   ```

2. Create a regression test:
   ```bash
   # Add the crash input to tests/fixtures/
   cp fuzz/artifacts/fuzz_config_loader/crash-* tests/fixtures/fuzz-crash-001
   ```

3. Report via GitHub Security Advisories (see above)

### Static Analysis

ThoughtJack is analyzed with multiple security tools:

- **Clippy** (pedantic + nursery lints) - Catches common Rust mistakes
- **cargo-deny** - Prevents vulnerable dependencies
- **CodeQL** - Semantic analysis for security vulnerabilities
- **OpenSSF Scorecard** - Supply chain security assessment

View results:
- CodeQL: https://github.com/thoughtgate/thoughtjack/security/code-scanning
- Scorecard: https://scorecard.dev/viewer/?uri=github.com/thoughtgate/thoughtjack

## Security Best Practices

### Running ThoughtJack Safely

ThoughtJack is an **offensive security tool** designed to simulate malicious behavior. Follow these guidelines:

1. **Isolation**: Always run in containers or VMs
   ```bash
   docker run --rm -it \
     -v $(pwd)/scenarios:/scenarios:ro \
     thoughtjack server run --config /scenarios/your-test.yaml
   ```

2. **Network Isolation**: Use network namespaces or firewall rules
   ```bash
   # Block outbound connections
   sudo iptables -A OUTPUT -m owner --uid-owner $(id -u) -j REJECT
   ```

3. **Resource Limits**: Configure generator limits
   ```yaml
   # scenarios/your-test.yaml
   metadata:
     generator_limits:
       max_payload_bytes: 1048576  # 1 MB
       max_nest_depth: 1000
   ```

4. **Never run against production systems**

### Contributing Security Tests

When adding new attack scenarios:

1. Document the attack vector in the scenario metadata
2. Set appropriate resource limits
3. Add fuzzing corpus entries if relevant
4. Test in isolated environment first
5. Include cleanup steps in documentation

Example:
```yaml
metadata:
  name: "Memory Exhaustion Test"
  description: "Tests client memory limits with large payloads"
  severity: "high"
  # IMPORTANT: Set limits to prevent accidental DoS
  generator_limits:
    max_payload_bytes: 10485760  # 10 MB (not 1 GB!)
```

## Security Audit History

- **2025-02**: Initial security features implemented (Sigstore signing, fuzzing, CodeQL)

## Acknowledgments

We appreciate responsible disclosure of security vulnerabilities. Contributors will be credited in release notes unless they prefer to remain anonymous.
