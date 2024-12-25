# rfind
[![CI](https://github.com/daviddl9/rfind/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/daviddl9/rfind/actions/workflows/ci.yml)

**rfind** is a **supercharged** alternative to the classic [\*nix `find`](https://man7.org/linux/man-pages/man1/find.1.html) command. It focuses on **speed**, **fuzzy matching**, and **smart caching** (via indexing). **rfind** accelerates searches by building and maintaining a local index for your most-used directories.

## Table of Contents
- [rfind](#rfind)
  - [Table of Contents](#table-of-contents)
  - [Features](#features)
  - [Installation](#installation)
  - [Usage](#usage)
  - [Project Structure](#project-structure)
  - [Testing](#testing)
  - [Continuous Integration](#continuous-integration)

---

## Features

- **Blazing-Fast Search**: Because of incremental indexing, most lookups are instant—even for large directories.  
- **Fuzzy Matching & Globbing**: Supports substring, partial matches, and standard glob patterns (`*`, `**`, `?`).  
- **Cross-Platform**: Linux, macOS, and Windows support (verified via GitHub Actions).  
- **Directory Hashing**: Quickly detects changes, re-indexing only when necessary (rather than rescanning everything).  
- **User-Friendly CLI**: Commands are intuitive and well-organized, making `rfind` easier to use than plain `find`.  

With **rfind**, you spend less time waiting for your system to comb through every file, and more time actually **using** the files you locate.

---

## Installation

1. **Install [Rust](https://www.rust-lang.org/tools/install)** (stable or newer).
2. Clone this repository:

   ```bash
   git clone https://github.com/daviddl9/rfind.git
   cd rfind
   ```

3. **Build and install**:

   ```bash
   cargo install --path .
   rfind --reindex
   ```
   
   This places the `rfind` binary in your Cargo bin directory (e.g. `~/.cargo/bin`).

Alternatively, run in-place (without installing) with:

```bash
cargo run -- <arguments-here>
```

---

## Usage

After installing, try:

```bash
rfind --help
```

**Basic commands**:

- **Index everything, then exit**:

  ```bash
  rfind --reindex
  ```

- **Force a reindex, then search for "chrome"**:

  ```bash
  rfind -f "chrome"
  ```

- **Search for all `.pdf` files** (glob search, like `find . -name '*.pdf'`):

  ```bash
  rfind "*.pdf"
  ```

- **Fuzzy search** for partial matches (e.g., "doc" might match "Documents"):

  ```bash
  rfind "doc"
  ```

- **Multi-term fuzzy search** (e.g., `'prsnl doc'` might match "Personal Documents"):

  ```bash
  rfind "prsnl doc"
  ```

> Compare this to manually typing `find ~/Documents -iname "*doc*"` for every search. **rfind** does the indexing up front, so actual lookups are lightning-quick.

---

## Project Structure

- **`src/`**  
  - **`main.rs`**: Entry point for the CLI tool (parses arguments, sets up `IndexManager`, handles printing).  
  - **`lib.rs`**: Exports library items, re-exporting modules for external (and test) use.  
  - **`index.rs`**: Core logic for indexing, fuzzy matching, searching, directory hashing, etc.  
- **`tests/`**  
  - **`integration_test.rs`**: Integration tests verifying indexing and searching with a temporary directory.  
- **`.github/workflows/ci.yml`**  
  - GitHub Actions workflow for building and testing on Ubuntu (Linux), macOS, and Windows.

---

## Testing

Run tests locally using:

```bash
cargo test
```

This will execute:

- **Unit tests** inside modules (e.g. in `src/index.rs` under `#[cfg(test)]`).  
- **Integration tests** in the `tests/` directory, compiled as separate crates.



## Continuous Integration

We use GitHub Actions to ensure that **rfind** builds and tests successfully across Linux, macOS, and Windows. The [workflow file](.github/workflows/ci.yml) includes:

1. **Check out** the repo code.  
2. **Install** Rust (stable).  
3. **Build** in `release` mode.  
4. **Run Tests** (unit + integration).  
5. **Upload Artifacts** (the resulting binary).  

A successful build across all three platforms ensures consistent behavior and superior reliability to `find`—everywhere.