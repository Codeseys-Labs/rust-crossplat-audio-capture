# Contributing to Audio Capture Library

## Getting Started

1. Fork the repository
2. Clone your fork
3. Create a new branch for your feature/fix

## Development Setup

### Prerequisites

- Rust toolchain (stable)
- Platform-specific dependencies:
  - Windows: Windows SDK
  - Linux: PulseAudio/PipeWire development libraries
  - macOS: Xcode Command Line Tools

### Building

```bash
# Build the library
cargo build

# Run tests
cargo test

# Run with specific features
cargo build --features "async wasapi"
```

## Code Style

- Follow Rust standard formatting (use `rustfmt`)
- Use meaningful variable names
- Add comments for non-obvious behavior
- Keep functions focused and small
- Use Rust idioms and best practices

## Testing

### Running Tests

```bash
# Run all tests
cargo test

# Run specific test categories
cargo test --test config_tests
cargo test --test capture_tests
cargo test --test async_tests

# Run with specific features
cargo test --features "async wasapi"
```

### Writing Tests

- Write unit tests for new functionality
- Include error case testing
- Use mock implementations where appropriate
- Follow existing test patterns

## Documentation

- Update relevant documentation for new features
- Include examples in doc comments
- Keep API documentation up to date
- Add platform-specific notes where needed

## Pull Request Process

1. Update documentation
2. Add/update tests
3. Ensure CI passes
4. Request review
5. Address feedback

## Commit Messages

Format:
```
[Component] Brief description

Detailed description of changes and reasoning.

Platform: [Windows/Linux/macOS/All]
```

Example:
```
[Windows] Add WASAPI device enumeration

Implements device enumeration for Windows using WASAPI.
Includes error handling and device filtering.

Platform: Windows
```

## Feature Requests

- Open an issue describing the feature
- Include use cases
- Discuss implementation approach
- Consider cross-platform implications

## Bug Reports

Include:
- Platform and version
- Steps to reproduce
- Expected vs actual behavior
- Relevant logs/output
- Minimal reproduction code if possible