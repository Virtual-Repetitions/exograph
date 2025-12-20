#!/bin/bash
# Prep script for pull requests - runs formatting and linting checks

set -e

echo "🔍 Running pre-PR checks..."
echo ""

echo "📝 Formatting code..."
cargo fmt --all
echo "✅ Formatting complete"
echo ""

echo "🔧 Running clippy..."
cargo clippy --all-targets --all-features -- -D warnings --no-deps
echo "✅ Clippy checks passed"
echo ""

echo "✨ All checks passed! Ready to commit and push."
