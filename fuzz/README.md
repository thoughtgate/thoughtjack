# Fuzzing ThoughtJack

This directory contains fuzz targets for security testing using [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) (libFuzzer).

## Fuzz Targets

1. **fuzz_jsonrpc_parser** - JSON-RPC message parsing, malformed structures
2. **fuzz_oatf_loader** - OATF YAML document parsing, preprocessing, SDK validation, cycle detection
3. **fuzz_mcp_handler** - MCP handler dispatch chain: request + state → response (18 handlers)
4. **fuzz_synthesize_validation** - Synthesized output validation with arbitrary protocol strings and JSON content
5. **fuzz_uri_template** - RFC 6570 Level 1 URI template matching with `{var}` expansion
6. **fuzz_mcp_response_dispatch** - OATF response selection, template interpolation, payload generation
7. **fuzz_a2a_sse_parser** - A2A SSE byte stream parsing, JSON-RPC result extraction
8. **fuzz_agui_sse_parser** - AG-UI SSE event+data parsing, event type mapping, multi-line data

## Running Locally

Install the nightly toolchain and cargo-fuzz:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

Run a specific target:

```bash
cd fuzz
cargo +nightly fuzz run fuzz_oatf_loader
```

Run with time limit (5 minutes):

```bash
cargo +nightly fuzz run fuzz_oatf_loader -- -max_total_time=300
```

List all targets:

```bash
cargo +nightly fuzz list
```

## Corpus Management

The corpus directories contain seed inputs for fuzzing. LibFuzzer will:
1. Start with these seed inputs
2. Mutate them to find new code paths
3. Save interesting inputs back to the corpus

### Updating the OATF Loader Corpus

The OATF loader corpus references YAML scenario files. To refresh it:

```bash
# From project root
./fuzz/scripts/update_corpus.sh
```

Or manually:

```bash
# Copy current scenarios to corpus
cp ../scenarios/*.yaml corpus/fuzz_oatf_loader/

# Remove duplicates (optional, fuzzer handles this)
cd corpus/fuzz_oatf_loader
for f in *; do
  md5sum "$f"
done | sort | uniq -w32 -D | cut -c35- | xargs rm -f
```

The corpus files are NOT tracked in git (they're gitignored) because libFuzzer manages them dynamically.

## Checking for Crashes

After fuzzing, check for crashes:

```bash
ls -la fuzz/artifacts/
```

If crashes are found, minimize them:

```bash
cargo +nightly fuzz tmin fuzz_oatf_loader artifacts/fuzz_oatf_loader/crash-*
```

Then create a regression test and report via GitHub Security Advisories.

## Coverage

View fuzzing coverage:

```bash
cargo +nightly fuzz coverage fuzz_oatf_loader
```

## CI Integration

Fuzzing runs nightly in GitHub Actions (`.github/workflows/security.yml`) for 8 hours total (1 hour per target).
Crashes are automatically reported as GitHub issues.
