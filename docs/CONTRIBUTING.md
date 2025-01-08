# Contributing Guidelines

## Getting Started

1. **Fork the Repository**
   * Click the 'Fork' button on GitHub
   * Clone your fork locally

2. **Set Up Development Environment**
   ```bash
   git clone https://github.com/daviddl9/rfind.git
   cd rfind
   cargo build
   ```

3. **Create a Branch**
   ```bash
   git checkout -b feature/your-feature-name
   ```

## Development Workflow

### Code Style

* Follow the Rust standard style guide
* Use `cargo fmt` before committing
* Run `cargo clippy` and address warnings
* Ensure all tests pass with `cargo test`

### Commits

* Use semantic commit messages:
  * feat: (new feature)
  * fix: (bug fix)
  * docs: (documentation changes)
  * style: (formatting, missing semicolons, etc)
  * refactor: (code changes that neither fix bugs nor add features)
  * test: (adding missing tests)
  * chore: (updating grunt tasks etc)

### Testing

* Write tests for new features
* Update tests for bug fixes
* Ensure all tests pass locally
* Add integration tests for new functionality

## Pull Request Process

1. Update documentation for any new features
2. Add or update tests as needed
3. Ensure CI passes
4. Get review from at least one maintainer
5. Squash commits before merge