# Pull Request Checklist

This document describes the PR workflow for the Exograph repository and how to validate your changes locally before submitting.

## Quick Start

Before creating a PR, run the pre-PR validation script:

```sh
npm run prepare:pr
```

This will run all critical checks locally to catch issues before they hit CI.

## What Gets Checked

### Critical Checks (Must Pass)

1. **Dependencies** - Verifies Rust, protoc, and Node.js are installed
2. **Node Modules** - Checks if npm dependencies are installed
3. **Code Formatting** - Runs `cargo fmt --all -- --check`
4. **Clippy Lints** - Runs `cargo clippy --all-targets --all-features -- -D warnings --no-deps`
5. **Build** - Compiles the project with `cargo build --workspace`
6. **Unit Tests** - Runs `cargo test --workspace`
7. **Integration Tests** - Runs `exo test integration-tests` with introspection tests enabled
8. **Error Reporting Tests** - Validates error reporting functionality (if Node.js available)

### Informational Checks (Warnings Only)

9. **Git Status** - Shows uncommitted changes
10. **Common Issues** - Checks for TODO/FIXME comments and Cargo.lock consistency

## GitHub Actions Workflow

When you create a PR, GitHub Actions will run the same checks across multiple platforms:

### Lint Job (Ubuntu)
- Formatting check (`cargo fmt`)
- Clippy lints
- Runs on every PR and push to main

### Test Job (Multi-platform)
- **Platforms**: Ubuntu 22.04, macOS 14, Windows 2022
- **Tests**:
  - Unit tests
  - Integration tests with introspection
  - Error reporting tests
- **Builds**: Targets `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`

### WASM Tests (Currently Disabled)
- WebAssembly tests are temporarily disabled due to tokio networking incompatibility

## Fixing Common Issues

### Code Formatting Failed

```sh
cargo fmt --all
# Or
npm run lint:fix
```

### Clippy Warnings

```sh
cargo clippy --all-targets --all-features -- -D warnings --no-deps
```

Review the warnings and fix them. Common issues:
- Unused variables (prefix with `_`)
- Unnecessary clones
- Missing documentation

### Unit Tests Failed

```sh
cargo test --workspace --exclude postgres-resolver-dynamic --exclude postgres-builder-dynamic --exclude server-cf-worker
```

Check the test output for specific failures. Run individual tests:

```sh
cargo test test_name
```

### Integration Tests Failed

Make sure PostgreSQL is running and properly configured:

```sh
# Check if postgres is running
pg_isready

# Run integration tests with full output
EXO_RUN_INTROSPECTION_TESTS=true ./target/debug/exo test integration-tests
```

### Build Failed

Clean and rebuild:

```sh
cargo clean
cargo build --workspace --exclude postgres-resolver-dynamic --exclude postgres-builder-dynamic --exclude server-cf-worker
```

## Manual Testing

Beyond automated checks, consider manual testing:

### Testing with a Sample Project

1. Navigate to a test project:
   ```sh
   cd integration-tests/basic-model-no-auth
   ```

2. Run in yolo mode (creates temp database):
   ```sh
   cargo run --bin exo yolo
   ```

3. Test in the GraphQL playground
4. Verify queries and mutations work as expected

### Testing Dev Mode

```sh
# Create a test database
createdb test-db

# Run dev mode
EXO_JWT_SECRET="test-secret" \
EXO_POSTGRES_URL=postgresql://localhost:5432/test-db \
EXO_POSTGRES_USER=$USER \
cargo run --bin exo dev
```

## Before Submitting Your PR

- [ ] Run `npm run prepare:pr` and ensure all critical checks pass
- [ ] Review your changes with `git diff`
- [ ] Write a clear PR description explaining what changed and why
- [ ] Add tests for new functionality
- [ ] Update documentation if needed
- [ ] Check that commit messages follow [conventional commits](https://www.conventionalcommits.org/)

## Commit Message Format

We use [commitlint](https://commitlint.js.org/) to enforce conventional commit messages. The format is:

```
type(scope): subject

body

footer
```

### Types
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, etc.)
- `refactor`: Code refactoring
- `test`: Adding or updating tests
- `chore`: Maintenance tasks
- `perf`: Performance improvements
- `ci`: CI/CD changes

### Examples

```
feat(postgres): add support for pgvector similarity search

Implements vector similarity search using the pgvector extension.
Adds new query operators for cosine and euclidean distance.

Closes #123
```

```
fix(cli): handle missing config file gracefully

Previously crashed with panic. Now shows helpful error message.
```

## CI Performance Tips

The GitHub Actions workflow can take 10+ minutes. To speed up feedback:

1. **Run checks locally first** - Catch issues before pushing
2. **Use draft PRs** - Mark PR as draft if still working on fixes
3. **Push complete changes** - Avoid multiple small commits that trigger CI
4. **Watch for platform-specific issues** - If tests pass locally but fail in CI, check the platform (Linux/macOS/Windows)

## Getting Help

- Check existing PRs for examples
- Review the [DEVELOPMENT.md](DEVELOPMENT.md) guide
- Ask in the team chat if stuck on a specific issue

## Common Scenarios

### Scenario: Tests pass locally but fail in CI

**Possible causes:**
- Platform differences (especially Windows)
- Missing dependencies in CI environment
- Timing issues in integration tests
- Postgres version differences

**Debug steps:**
1. Check which platform failed in the workflow
2. Review the full CI logs for that platform
3. Look for environment-specific errors
4. Consider adding platform-specific handling if needed

### Scenario: Clippy passes locally but fails in CI

**Possible causes:**
- Different Rust versions
- Different clippy version
- Feature flags affecting code paths

**Debug steps:**
1. Update Rust toolchain: `rustup update`
2. Run with same flags as CI: `cargo clippy --all-targets --all-features -- -D warnings --no-deps`
3. Check if you're on a different branch that has different clippy config

### Scenario: Integration tests are slow

**Tips:**
- Run specific test directories: `./target/debug/exo test integration-tests/basic-model-no-auth`
- Use `cargo test --lib` for unit tests only
- Keep PostgreSQL running between test runs
- Consider using `cargo watch` during development

## Workflow Optimization

### During Active Development

```sh
# Terminal 1: Watch and rebuild on changes
cargo watch -c -x 'build --workspace --exclude postgres-resolver-dynamic --exclude postgres-builder-dynamic --exclude server-cf-worker'

# Terminal 2: Run specific tests as needed
cargo test specific_test_name
```

### Before Committing

```sh
# Quick validation (just formatting and clippy)
npm run lint

# Full validation
npm run prepare:pr
```

### Before Pushing to GitHub

```sh
# Final check
npm run prepare:pr

# Review changes
git diff main...HEAD

# Push
git push origin your-branch
```
