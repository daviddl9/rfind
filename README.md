Below is an updated `README.md` with additional usage examples illustrating how to leverage the `-H`, `-L`, `-P`, and `-t` flags. It also includes a few small clarifications and improvements you might find useful.

---

# 🚀 rfind
[![CI](https://github.com/daviddl9/rfind/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/daviddl9/rfind/actions/workflows/ci.yml)

A blazingly fast parallel recursive file finder that supports both glob patterns and substring search. It is a **supercharged** alternative to the classic [\*nix `find`](https://man7.org/linux/man-pages/man1/find.1.html) command, written in Rust.

## ✨ Features

* 🔍 Supports both glob patterns (e.g., `*.log`) and substring search  
* ⚡ Parallel processing utilizing all CPU cores for maximum performance  
* 🎯 Up to 10x faster than traditional `find` command in large directory structures  
* 🌲 Configurable maximum search depth  
* 🧵 Customizable thread count  
* 🌐 Configurable symlink handling (`-P`, `-H`, `-L`)  
* 🎨 Colorized output for better readability  

## 💨 Performance

In benchmarks on large directory structures (1M+ files), rfind consistently outperforms the traditional UNIX `find` command:

* 🏃 Directory with 1M files: `find` takes ~45s, rfind completes in ~4s  
* 📁 Deep nested structures: Up to 12x performance improvement  
* 💾 SSD optimization: Maximizes I/O throughput with parallel workers  

## 🛠️ Usage

```bash
Parallel recursive file finder

Usage: rfind [OPTIONS] <PATTERN>

Arguments:
  <PATTERN>  Pattern to search for (glob patterns like *.log or substring search)

Options:
  -d, --dir <DIR>              Starting directory (defaults to root directory) [default: /]
  -m, --max-depth <MAX_DEPTH>  Maximum search depth [default: 100]
  -j, --threads <THREADS>      Number of worker threads (defaults to number of CPU cores)
  -P, --no-follow              Never follow symbolic links (default)
  -H, --cmd-follow             Follow symbolic links on command line only
  -L, --follow-all             Follow all symbolic links
  -t, --type <TYPE_FILTER>     Filter the results by type. Possible values: f|file, d|dir, l|symlink, or any [default: any]
  -h, --help                   Print help
  -V, --version                Print version
```

## 📝 Examples

### Basic Searches

- **Search for all log files in the current directory:**
  ```bash
  rfind "*.log" -d .
  ```
  *(You can omit `-d .` if you want to start in the current directory, but by default `rfind` starts at `/`.)*

- **Find all Python files up to 3 directories deep:**
  ```bash
  rfind -m 3 "*.py"
  ```

- **Search for files containing "backup" in their name:**
  ```bash
  rfind "backup"
  ```

### Symbolic Link Handling

- **Never follow symlinks** (default: `-P`):  
  ```bash
  rfind -P "*.conf"
  ```
  This ensures that **no** symbolic links are traversed.  

- **Follow symlinks on the command line only** (`-H`):  
  ```bash
  # Suppose /path/to/symlink is itself a symlink. Only that link is followed, no others.
  rfind -H -d /path/to/symlink "*.log"
  ```
  This is useful if you only want to follow a specific symlink passed directly as an argument, but ignore any symlinks you encounter further down the directory tree.

- **Follow all symlinks** (`-L`):  
  ```bash
  rfind -L -d /var/www "*.html"
  ```
  This will recursively follow every symlink encountered, which can be useful for large codebases or multi-directory dev environments. Use with caution to avoid infinite loops if there are circular symlinks (rfind does detect and avoid most loops by keeping track of visited paths).

### Filtering by Type

Use `-t` (or `--type`) to filter results by file type:

- **Only files**:
  ```bash
  rfind -t f "report"
  ```
  This will return only regular files whose names match "report" (substring or glob).

- **Only directories**:
  ```bash
  rfind -t d "backup"
  ```
  This is handy if you’re specifically looking for directories named or containing “backup”.

- **Only symlinks**:
  ```bash
  rfind -t l "data"
  ```
  Returns only symlinks matching "data".  

- **Any type** (default):
  ```bash
  rfind -t any "*test*"
  ```
  Shows both files, directories, and symlinks that have "test" in their name.

## 🔧 Installation

1. **Install [Rust](https://www.rust-lang.org/tools/install)** (stable or newer).  
2. Clone this repository:

   ```bash
   git clone https://github.com/daviddl9/rfind.git
   cd rfind
   ```

3. **Build and install**:

   ```bash
   cargo build --release
   cargo install --path .
   ```
   This places the `rfind` binary in your Cargo bin directory (e.g. `~/.cargo/bin`).

## ⚡ How It Works

rfind achieves its exceptional performance through:

- Multi-threaded directory traversal  
- Efficient work distribution using crossbeam channels  
- Smart memory management with pre-allocated buffers  
- Zero-copy string matching  
- Adaptive thread pooling  

## Additional Suggestions

- **Avoiding hidden files or directories**: Currently, `rfind` doesn’t provide a built-in flag to ignore `.*` entries. For now, you can combine `rfind` with standard shell utilities like `grep` or `sed` to filter results if you need to exclude hidden files:
  ```bash
  rfind "*.rs" | grep -v "/\."
  ```
  A future enhancement might include a built-in `--ignore-hidden` flag.

- **Excluding specific directories**: Similar to ignoring hidden files, you can pipe `rfind` results to `grep -v` for rudimentary exclusions:
  ```bash
  rfind "*.log" | grep -v "node_modules"
  ```

## 📄 License

[MIT License](https://github.com/daviddl9/rfind/blob/main/LICENSE)

## 🤝 Contributing

Contributions are welcome! Feel free to submit issues and pull requests. If you encounter a bug, open an issue with steps to reproduce. If you have ideas for improvements, we’d love to hear them.