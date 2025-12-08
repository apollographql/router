#!/usr/bin/env bash
# This script validates MDX files in the docs directory
# Usage: ./scripts/validate-mdx.sh [file_or_directory]
#
# If no argument is provided, it validates all .mdx files in docs/source
# If a file is provided, it validates only that file
# If a directory is provided, it validates all .mdx files in that directory
#
# Requirements: Node.js and npm must be installed
# MDX version: @mdx-js/mdx@^3.0.0
#
# Note: This script installs @mdx-js/mdx in a temporary directory for each run
# to avoid requiring global installation. The temp directory is cleaned up on exit.

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Check if npm is installed
if ! command -v npm &> /dev/null; then
    echo -e "${RED}Error: npm is required but not installed${NC}"
    exit 1
fi

# Store original directory before creating temp
ORIG_DIR="$PWD"

# Create temporary directory for validation
TEMP_DIR=$(mktemp -d)
trap "rm -rf $TEMP_DIR" EXIT

# Set up validation package
pushd "$TEMP_DIR" > /dev/null
npm init -y > /dev/null 2>&1
npm install --silent @mdx-js/mdx > /dev/null 2>&1

# Create validation script
cat > validate.js << 'EOF'
const fs = require('fs');
const { compile } = require('@mdx-js/mdx');

async function validateMdx(filepath) {
  try {
    const content = fs.readFileSync(filepath, 'utf8');
    // Use compile without deprecated jsx option
    await compile(content);
    return { valid: true };
  } catch (error) {
    return { 
      valid: false, 
      error: error.message,
      position: error.position,
      line: error.position?.start?.line,
      column: error.position?.start?.column
    };
  }
}

const filepath = process.argv[2];
validateMdx(filepath).then(result => {
  console.log(JSON.stringify(result));
  process.exit(result.valid ? 0 : 1);
});
EOF
popd > /dev/null

# Determine what to validate
TARGET="${1:-docs/source}"

# Make target path absolute if it's relative
if [[ "$TARGET" != /* ]]; then
    TARGET="$ORIG_DIR/$TARGET"
fi

# Find all .mdx files
if [ -f "$TARGET" ]; then
    FILES=("$TARGET")
elif [ -d "$TARGET" ]; then
    mapfile -t FILES < <(find "$TARGET" -name "*.mdx")
else
    echo -e "${RED}Error: $TARGET is not a valid file or directory${NC}"
    exit 1
fi

if [ ${#FILES[@]} -eq 0 ]; then
    echo -e "${YELLOW}No .mdx files found in $TARGET${NC}"
    exit 0
fi

# Validate each file
echo "Validating ${#FILES[@]} MDX file(s)..."
FAILED=0

for file in "${FILES[@]}"; do
    # Make relative path for display
    DISPLAY_FILE="${file#$ORIG_DIR/}"
    
    RESULT=$(cd "$TEMP_DIR" && node validate.js "$file" 2>&1 || true)
    
    # Parse JSON result properly
    if echo "$RESULT" | grep -q '"valid":true'; then
        echo -e "${GREEN}✓${NC} $DISPLAY_FILE"
    else
        echo -e "${RED}✗${NC} $DISPLAY_FILE"
        # Extract error message from JSON
        ERROR_MSG=$(echo "$RESULT" | grep -o '"error":"[^"]*"' | sed 's/"error":"//; s/"$//' | sed 's/\\"/"/g')
        if [ -n "$ERROR_MSG" ]; then
            echo "  Error: $ERROR_MSG"
        else
            echo "  Error: Unable to parse validation error"
        fi
        FAILED=$((FAILED + 1))
    fi
done

# Summary
echo ""
if [ $FAILED -eq 0 ]; then
    echo -e "${GREEN}All MDX files are valid!${NC}"
    exit 0
else
    echo -e "${RED}$FAILED MDX file(s) failed validation${NC}"
    exit 1
fi
