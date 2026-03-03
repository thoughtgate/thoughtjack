#!/usr/bin/env bash
# Update fuzzing corpus with current scenario files

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FUZZ_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$FUZZ_DIR")"

echo "Updating fuzz corpus from scenarios..."

# OATF loader corpus: copy all YAML scenarios
echo "  - Updating fuzz_oatf_loader corpus"
mkdir -p "$FUZZ_DIR/corpus/fuzz_oatf_loader"
cp -f "$PROJECT_ROOT/scenarios"/*.yaml "$FUZZ_DIR/corpus/fuzz_oatf_loader/"

# Count files
OATF_COUNT=$(find "$FUZZ_DIR/corpus/fuzz_oatf_loader" -name "*.yaml" | wc -l)
echo "    ✓ Added $OATF_COUNT YAML scenarios"

echo "Done!"
echo ""
echo "To run fuzzing with updated corpus:"
echo "  cd fuzz"
echo "  cargo +nightly fuzz run fuzz_oatf_loader"
