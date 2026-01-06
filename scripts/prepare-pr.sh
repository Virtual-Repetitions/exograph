#!/bin/bash

# Pre-PR validation script for exograph
# Runs critical checks locally before pushing to GitHub to catch issues early

set -e

# Color codes
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Track results
CRITICAL_FAILURES=0
WARNINGS=0

echo "========================================="
echo "Exograph Pre-PR Validation"
echo "========================================="
echo ""

# Helper functions
check_pass() {
  echo -e "${GREEN}✓${NC} $1"
}

check_fail() {
  echo -e "${RED}✗${NC} $1"
  CRITICAL_FAILURES=$((CRITICAL_FAILURES + 1))
}

check_warn() {
  echo -e "${YELLOW}⚠${NC} $1"
  WARNINGS=$((WARNINGS + 1))
}

# 1. Check dependencies
echo "1. Checking dependencies..."
if ! command -v cargo &> /dev/null; then
  check_fail "Cargo not found. Install Rust from https://rustup.rs/"
  exit 1
fi

if ! command -v protoc &> /dev/null; then
  check_warn "protoc not found. Install protobuf-compiler"
  echo "  Builds and tests may fail without protoc"
fi

if ! command -v node &> /dev/null; then
  check_warn "Node.js not found. Some tests may not run."
fi

if command -v protoc &> /dev/null && command -v node &> /dev/null; then
  check_pass "Dependencies installed"
fi

# 2. Check for Node dependencies (for error-report-testing)
echo ""
echo "2. Checking Node.js dependencies..."
if [ ! -d "node_modules" ]; then
  check_warn "Node modules not installed. Run 'npm install' to enable commit message validation."
  echo "  Fix: npm install"
else
  check_pass "Node modules installed"
fi

# 3. Formatting check
echo ""
echo "3. Checking code formatting..."
if cargo fmt --all -- --check > /dev/null 2>&1; then
  check_pass "Code formatting passed"
else
  check_fail "Code formatting failed"
  echo "  Fix: cargo fmt --all"
  echo "  Or: npm run lint:fix"
fi

# 4. Clippy lints
echo ""
echo "4. Running Clippy lints..."
CLIPPY_OUTPUT=$(cargo clippy --all-targets --all-features -- -D warnings --no-deps 2>&1)
CLIPPY_EXIT=$?

if [ $CLIPPY_EXIT -eq 0 ]; then
  check_pass "Clippy checks passed"
else
  check_fail "Clippy found issues"
  echo "$CLIPPY_OUTPUT" | grep -E "^(error|warning):" | head -10
  echo "  Fix issues and run: cargo clippy --all-targets --all-features -- -D warnings --no-deps"
fi

# 5. Build
echo ""
echo "5. Building project..."
if cargo build --workspace --exclude postgres-resolver-dynamic --exclude postgres-builder-dynamic --exclude server-cf-worker > /dev/null 2>&1; then
  check_pass "Build succeeded"
else
  check_fail "Build failed"
  echo "  Fix: cargo build --workspace --exclude postgres-resolver-dynamic --exclude postgres-builder-dynamic --exclude server-cf-worker"
fi

# 6. Unit tests
echo ""
echo "6. Running unit tests..."
TEST_OUTPUT=$(cargo test --workspace --exclude postgres-resolver-dynamic --exclude postgres-builder-dynamic --exclude server-cf-worker 2>&1)
TEST_EXIT=$?

if [ $TEST_EXIT -eq 0 ]; then
  # Count test results
  TEST_COUNT=$(echo "$TEST_OUTPUT" | grep -oE "test result: ok\. [0-9]+ passed" | grep -oE "[0-9]+" | head -1)
  if [ -n "$TEST_COUNT" ]; then
    check_pass "Unit tests passed ($TEST_COUNT tests)"
  else
    check_pass "Unit tests passed"
  fi
else
  check_fail "Unit tests failed"
  echo "$TEST_OUTPUT" | grep -A 5 "FAILED" | head -20
  echo "  Fix: cargo test --workspace --exclude postgres-resolver-dynamic --exclude postgres-builder-dynamic --exclude server-cf-worker"
fi

# 7. Integration tests
echo ""
echo "7. Running integration tests..."
if [ -f "target/debug/exo" ] || [ -f "target/debug/exo.exe" ]; then
  EXO_BIN="./target/debug/exo"
  if [ "$OSTYPE" = "msys" ] || [ "$OSTYPE" = "win32" ]; then
    EXO_BIN="${EXO_BIN}.exe"
  fi
  
  INTEGRATION_OUTPUT=$(EXO_RUN_INTROSPECTION_TESTS=true "$EXO_BIN" test integration-tests 2>&1)
  INTEGRATION_EXIT=$?
  
  if [ $INTEGRATION_EXIT -eq 0 ]; then
    check_pass "Integration tests passed"
  else
    check_fail "Integration tests failed"
    echo "$INTEGRATION_OUTPUT" | grep -E "FAILED|Error" | head -10
    echo "  Fix: EXO_RUN_INTROSPECTION_TESTS=true ./target/debug/exo test integration-tests"
  fi
else
  check_warn "Exo binary not found. Build first to run integration tests."
fi

# 8. Error reporting tests (if Node.js is available)
echo ""
echo "8. Running error reporting tests..."
if command -v node &> /dev/null && [ -d "error-report-testing" ]; then
  cd error-report-testing
  if [ ! -d "node_modules" ]; then
    check_warn "Error reporting test dependencies not installed"
    echo "  Fix: cd error-report-testing && npm install"
  else
    EXO_BIN="../target/debug/exo"
    if [ "$OSTYPE" = "msys" ] || [ "$OSTYPE" = "win32" ]; then
      EXO_BIN="${EXO_BIN}.exe"
    fi
    
    if EXO_EXECUTABLE="$EXO_BIN" npm run dev > /dev/null 2>&1; then
      check_pass "Error reporting tests passed"
    else
      check_warn "Error reporting tests failed (non-critical)"
      echo "  This may require manual review"
    fi
  fi
  cd ..
else
  check_warn "Error reporting tests skipped (Node.js not available or directory missing)"
fi

# 9. Git status check
echo ""
echo "9. Checking git status..."
if [ -d ".git" ]; then
  if git diff --quiet && git diff --cached --quiet; then
    check_warn "No uncommitted changes detected"
  else
    check_warn "You have uncommitted changes. Remember to commit them."
    git status --short | head -10
  fi
else
  check_warn "Not a git repository"
fi

# 10. Check for common issues
echo ""
echo "10. Checking for common issues..."

# Check for TODO/FIXME comments in staged files
if [ -d ".git" ]; then
  STAGED_FILES=$(git diff --cached --name-only --diff-filter=ACM | grep -E '\.(rs|toml)$' || true)
  if [ -n "$STAGED_FILES" ]; then
    TODO_COUNT=$(echo "$STAGED_FILES" | xargs grep -i "TODO\|FIXME" 2>/dev/null | wc -l || true)
    if [ "$TODO_COUNT" -gt 0 ]; then
      check_warn "Found $TODO_COUNT TODO/FIXME comments in staged files"
      echo "  Review these before committing"
    fi
  fi
fi

# Check cargo.lock is committed if Cargo.toml changed
if [ -d ".git" ]; then
  if git diff --cached --name-only | grep -q "Cargo.toml"; then
    if ! git diff --cached --name-only | grep -q "Cargo.lock"; then
      check_warn "Cargo.toml changed but Cargo.lock not staged"
      echo "  Consider staging Cargo.lock: git add Cargo.lock"
    fi
  fi
fi

check_pass "Common checks completed"

# Summary
echo ""
echo "========================================="
echo "Summary"
echo "========================================="

if [ $CRITICAL_FAILURES -eq 0 ]; then
  echo -e "${GREEN}✓ All critical checks passed!${NC}"
  if [ $WARNINGS -gt 0 ]; then
    echo -e "${YELLOW}⚠ $WARNINGS warning(s) - review above${NC}"
  fi
  echo ""
  echo "You're ready to create a PR!"
  exit 0
else
  echo -e "${RED}✗ $CRITICAL_FAILURES critical check(s) failed${NC}"
  if [ $WARNINGS -gt 0 ]; then
    echo -e "${YELLOW}⚠ $WARNINGS warning(s)${NC}"
  fi
  echo ""
  echo "Please fix the issues above before creating a PR."
  exit 1
fi
