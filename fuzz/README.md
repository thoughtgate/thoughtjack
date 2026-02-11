# Fuzzing ThoughtJack

This directory contains fuzz targets for security testing using [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) (libFuzzer).

## Fuzz Targets

1. **fuzz_config_loader** - YAML configuration parsing, directives, recursion limits
2. **fuzz_jsonrpc_parser** - JSON-RPC message parsing, malformed structures
3. **fuzz_phase_trigger** - Trigger evaluation, regex patterns, content matching
4. **fuzz_nested_json_generator** - Generator limits, stack overflow prevention

## Running Locally

Install the nightly toolchain and cargo-fuzz:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

Run a specific target:

```bash
cd fuzz
cargo +nightly fuzz run fuzz_config_loader
```

Run with time limit (5 minutes):

```bash
cargo +nightly fuzz run fuzz_config_loader -- -max_total_time=300
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

### Updating the Config Loader Corpus

The config loader corpus references YAML scenario files. To refresh it:

```bash
# From project root
./fuzz/scripts/update_corpus.sh
```

Or manually:

```bash
# Copy current scenarios to corpus
cp ../scenarios/*.yaml corpus/fuzz_config_loader/

# Remove duplicates (optional, fuzzer handles this)
cd corpus/fuzz_config_loader
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
cargo +nightly fuzz tmin fuzz_config_loader artifacts/fuzz_config_loader/crash-*
```

Then create a regression test and report via GitHub Security Advisories.

## Coverage

View fuzzing coverage:

```bash
cargo +nightly fuzz coverage fuzz_config_loader
```

## CI Integration

Fuzzing runs nightly in GitHub Actions (`.github/workflows/security.yml`) for 4 hours total.
Crashes are automatically reported as GitHub issues.
