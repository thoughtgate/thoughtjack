#!/usr/bin/env bash
# Update fuzzing corpus with current scenario files

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FUZZ_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$FUZZ_DIR")"

echo "Updating fuzz corpus from scenarios..."

# Config loader corpus: copy all YAML scenarios
echo "  - Updating fuzz_config_loader corpus"
mkdir -p "$FUZZ_DIR/corpus/fuzz_config_loader"
cp -f "$PROJECT_ROOT/scenarios"/*.yaml "$FUZZ_DIR/corpus/fuzz_config_loader/"

# Count files
CONFIG_COUNT=$(find "$FUZZ_DIR/corpus/fuzz_config_loader" -name "*.yaml" | wc -l)
echo "    ✓ Added $CONFIG_COUNT YAML scenarios"

echo "Done!"
echo ""
echo "To run fuzzing with updated corpus:"
echo "  cd fuzz"
echo "  cargo +nightly fuzz run fuzz_config_loader"
