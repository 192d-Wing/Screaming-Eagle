# Contributing to Screaming Eagle CDN

Thank you for your interest in contributing to Screaming Eagle CDN! This document provides guidelines and instructions for contributing.

## Table of Contents

- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Making Changes](#making-changes)
- [Testing](#testing)
- [Code Style](#code-style)
- [Commit Guidelines](#commit-guidelines)
- [Pull Request Process](#pull-request-process)
- [Reporting Bugs](#reporting-bugs)
- [Documentation](#documentation)
- [Community](#community)

### Our Standards

- Be respectful of differing viewpoints and experiences
- Gracefully accept constructive criticism
- Focus on what is best for the community
- Show empathy towards other community members

## Getting Started

### Prerequisites

Before contributing, ensure you have:

- **Rust 1.92+** installed ([rustup.rs](https://rustup.rs/))
- **Git** for version control
- **Docker** (optional, for testing)
- Familiarity with Rust, async programming, and HTTP

### Fork and Clone

1. **Fork the repository** on GitHub
2. **Clone your fork:**

   ```bash
   git clone https://github.com/your-username/Screaming-Eagle.git
   cd Screaming-Eagle
   ```

3. **Add upstream remote:**

   ```bash
   git remote add upstream https://github.com/original-owner/Screaming-Eagle.git
   ```

## Development Setup

### Install Dependencies

```bash
# Install Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install development tools
rustup component add clippy rustfmt

# Build the project
cargo build
```

### Run Locally

```bash
# Run with default config
cargo run

# Run with custom config
cargo run -- --config config/cdn.toml

# Run with debug logging
RUST_LOG=debug cargo run

# Run tests
cargo test
```

### Development Tools

**Recommended tools:**

```bash
# Install cargo-watch for auto-recompilation
cargo install cargo-watch

# Watch and run on changes
cargo watch -x run

# Install cargo-edit for dependency management
cargo install cargo-edit

# Add a dependency
cargo add tokio
```

## Making Changes

### Workflow

1. **Create a feature branch:**

   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make your changes:**
   - Write code
   - Add tests
   - Update documentation

3. **Test your changes:**

   ```bash
   cargo test
   cargo clippy
   cargo fmt
   ```

4. **Commit your changes:**

   ```bash
   git add .
   git commit -m "feat: add your feature description"
   ```

5. **Push to your fork:**

   ```bash
   git push origin feature/your-feature-name
   ```

6. **Open a Pull Request** on GitHub

### Keeping Your Fork Updated

```bash
# Fetch upstream changes
git fetch upstream

# Merge upstream main into your branch
git checkout main
git merge upstream/main

# Rebase your feature branch
git checkout feature/your-feature-name
git rebase main
```

## Testing

### Running Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_name

# Run with output
cargo test -- --nocapture

# Run integration tests only
cargo test --test integration_tests
```

### Writing Tests

**Unit tests** go in the same file as the code:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_insert() {
        let cache = Cache::new(CacheConfig::default());
        cache.insert("key".to_string(), response);
        assert!(cache.get("key").is_some());
    }

    #[tokio::test]
    async fn test_async_handler() {
        let result = cdn_handler(request).await;
        assert!(result.is_ok());
    }
}
```

**Integration tests** go in `tests/`:

```rust
// tests/integration_tests.rs
use screaming_eagle::*;

#[tokio::test]
async fn test_full_request_flow() {
    let app = create_app(config).await;
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
}
```

### Test Coverage

Aim for:

- 80%+ code coverage overall
- 100% coverage for critical paths (cache, circuit breaker, rate limiter)
- Integration tests for major features

```bash
# Install tarpaulin for coverage
cargo install cargo-tarpaulin

# Generate coverage report
cargo tarpaulin --out Html
```

## Code Style

### Formatting

Use `rustfmt` for consistent formatting:

```bash
# Format all code
cargo fmt

# Check formatting without modifying
cargo fmt -- --check
```

### Linting

Use `clippy` for linting:

```bash
# Run clippy
cargo clippy

# Run clippy with strict warnings
cargo clippy -- -D warnings
```

### Style Guidelines

**General:**

- Use meaningful variable names
- Keep functions small and focused
- Add comments for complex logic
- Use `Result` for error handling, not `panic!`

**Rust-specific:**

- Prefer `&str` over `String` for function parameters
- Use `impl Trait` for return types when appropriate
- Avoid unnecessary clones
- Use `Arc` for shared state, not `Rc`
- Prefer `async fn` over `impl Future`

**Example:**

```rust
// Good
pub async fn fetch_origin(
    client: &Client,
    url: &str,
) -> Result<Response, Error> {
    client.get(url)
        .send()
        .await
        .map_err(|e| Error::OriginFetch(e))
}

// Avoid
pub async fn fetch_origin(client: Client, url: String) -> Response {
    client.get(&url).send().await.unwrap() // Don't unwrap!
}
```

### Code Documentation

Document public APIs with doc comments:

```rust
/// Fetches a resource from the origin server.
///
/// # Arguments
///
/// * `url` - The URL to fetch
/// * `timeout` - Request timeout in seconds
///
/// # Returns
///
/// Returns the response body as bytes.
///
/// # Errors
///
/// Returns an error if the request fails or times out.
///
/// # Example
///
/// ```
/// let body = fetch_origin("https://example.com", 30).await?;
/// ```
pub async fn fetch_origin(url: &str, timeout: u64) -> Result<Bytes, Error> {
    // ...
}
```

## Commit Guidelines

### Commit Message Format

Use [Conventional Commits](https://www.conventionalcommits.org/):

```text
<type>(<scope>): <subject>

<body>

<footer>
```

**Types:**

- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, etc.)
- `refactor`: Code refactoring
- `perf`: Performance improvements
- `test`: Adding or updating tests
- `chore`: Maintenance tasks

**Examples:**

```text
feat(cache): add LRU-K eviction policy

Implement LRU-K algorithm for better cache hit ratios.
Tracks access frequency and recency for each entry.

Closes #123
```

```text
fix(circuit-breaker): prevent race condition in state transitions

Use atomic operations for state changes to avoid race conditions
when multiple requests trigger state transitions simultaneously.
```

```text
docs(api): add examples for cache purging endpoints

Add curl examples and response samples to API documentation.
```

### Commit Best Practices

- Make atomic commits (one logical change per commit)
- Write descriptive commit messages
- Reference issue numbers when applicable
- Keep commits focused and small

## Pull Request Process

### Before Submitting

Ensure your PR:

1. **Passes all tests:**

   ```bash
   cargo test
   ```

2. **Passes linting:**

   ```bash
   cargo clippy -- -D warnings
   ```

3. **Is properly formatted:**

   ```bash
   cargo fmt -- --check
   ```

4. **Updates documentation** if needed

5. **Includes tests** for new features

### PR Description Template

```markdown
## Description

Brief description of changes

## Type of Change

- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change
- [ ] Documentation update

## Testing

How has this been tested?

## Checklist

- [ ] Tests pass locally
- [ ] Code follows style guidelines
- [ ] Documentation updated
- [ ] No breaking changes (or documented if necessary)
- [ ] Changelog updated

## Related Issues

Closes #123
Related to #456
```

### Review Process

1. **Submit PR** with descriptive title and description
2. **Maintainers review** within 1-2 weeks
3. **Address feedback** by pushing new commits
4. **Approval** from at least one maintainer required
5. **Merge** after approval and CI passes

### After Merge

- Delete your feature branch
- Update your local repository
- Celebrate your contribution! 🎉

## Reporting Bugs

### Before Reporting

- Search existing issues to avoid duplicates
- Test with the latest version
- Verify it's not a configuration issue

### Bug Report Template

```markdown
## Description

Clear description of the bug

## Steps to Reproduce

1. Configure CDN with...
2. Send request to...
3. Observe error...

## Expected Behavior

What should happen

## Actual Behavior

What actually happens

## Environment

- OS: Ubuntu 22.04
- Rust version: 1.75.0
- CDN version: 1.0.0
- Deployment: Docker

## Configuration

```toml
[server]
port = 8080
# ... (sanitize sensitive data)
```

## Logs

```text
[2026-01-18T12:00:00Z ERROR] ...
```

## Additional Context

Screenshots, metrics, etc.

```text

## Suggesting Features

### Feature Request Template

```markdown
## Problem Statement

What problem does this solve?

## Proposed Solution

Describe the feature

## Alternatives Considered

Other approaches you've thought of

## Implementation Ideas

If you have implementation ideas

## Additional Context

Examples, mockups, etc.
```

### Feature Discussion

- Open an issue for discussion before implementing large features
- Be open to feedback and alternative approaches
- Consider backward compatibility
- Think about performance implications

## Documentation

### Types of Documentation

1. **Code documentation:** Doc comments for public APIs
2. **README:** High-level overview and quick start
3. **Architecture docs:** System design and internals
4. **API reference:** Complete API documentation
5. **Guides:** Deployment, configuration, troubleshooting

### Updating Documentation

When you change code, update:

- Inline doc comments
- README if user-facing changes
- API documentation for endpoint changes
- Configuration reference for new options
- Architecture docs for design changes

### Documentation Style

- Use clear, concise language
- Include examples
- Explain the "why" not just the "what"
- Keep it up-to-date with code changes

## Community

### Communication Channels

- **GitHub Issues:** Bug reports, feature requests
- **GitHub Discussions:** Questions, ideas, general discussion
- **Pull Requests:** Code contributions and reviews

### Getting Help

- Read the documentation first
- Search existing issues and discussions
- Ask in GitHub Discussions for questions
- Be specific and provide context

### Recognition

Contributors are recognized in:

- CONTRIBUTORS.md file
- Release notes
- GitHub contributors page

## Development Tips

### Debugging

```bash
# Run with debug logging
RUST_LOG=debug cargo run

# Run with trace logging for specific module
RUST_LOG=screaming_eagle::cache=trace cargo run

# Use rust-gdb for debugging
rust-gdb target/debug/screaming-eagle-cdn
```

### Performance Testing

```bash
# Load testing with hey
hey -n 10000 -c 100 http://localhost:8080/example/test.html

# Benchmarking
cargo bench

# Profiling
cargo flamegraph
```

### Common Tasks

**Add a new origin configuration option:**

1. Update `config.rs` struct
2. Update default configuration
3. Use the option in `origin.rs`
4. Add tests
5. Update documentation

**Add a new metric:**

1. Add metric to `metrics.rs`
2. Instrument code to record metric
3. Add to Prometheus export
4. Document in API reference

**Add a new endpoint:**

1. Add handler in `handlers.rs`
2. Add route in `main.rs`
3. Add authentication if needed
4. Add tests
5. Document in API reference

## Questions?

If you have questions about contributing:

- Open a GitHub Discussion
- Review existing issues and PRs
- Read the documentation
- Ask in your PR or issue

Thank you for contributing to Screaming Eagle CDN! 🦅
