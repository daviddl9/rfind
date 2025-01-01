use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(windows)]
use std::os::windows::fs::symlink_file as symlink;
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::TempDir;

/// Represents a single integration test configuration
struct TestCase {
    pattern: &'static str,
    /// List of (file_name, expected_count). For example:
    ///   vec![("test6.log", 1), ("link_to_test6.log", 1)]
    /// means we expect test6.log once and link_to_test6.log once.
    expected_counts: Vec<(&'static str, usize)>,
    max_depth: Option<usize>,
    threads: Option<usize>,
    /// "f", "d", "l", or "any" (or None to omit the --type arg)
    type_filter: Option<&'static str>,
    /// Symlink mode, e.g. Some("-H"), Some("-L"), or None (use default -P)
    symlink_mode: Option<&'static str>,
    description: &'static str,
}

/// Convert a slice of (file_name, count) into a HashMap.
fn make_expected_map(items: &[(&str, usize)]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for (name, count) in items {
        map.insert(name.to_string(), *count);
    }
    map
}

/// Example integration test
#[test]
fn test_file_finder_integration() -> Result<(), Box<dyn std::error::Error>> {
    // Create a temporary directory structure for testing
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path();

    // Create directories
    let test_dirs = [
        "dir1/subdir1",
        "dir1/subdir2",
        "dir2/subdir1",
        "dir3/subdir1/subsubdir1",
    ];
    for dir in test_dirs.iter() {
        fs::create_dir_all(base_path.join(dir))?;
    }

    // Create files
    let test_files = [
        ("dir1/test1.txt", "content1"),
        ("dir1/subdir1/test2.log", "content2"),
        ("dir1/subdir2/test3.txt", "content3"),
        ("dir2/test4.log", "content4"),
        ("dir2/subdir1/test5.txt", "content5"),
        ("dir3/subdir1/test6.log", "content6"),
        ("dir3/subdir1/subsubdir1/test7.txt", "content7"),
    ];
    for (path, content) in test_files.iter() {
        fs::write(base_path.join(path), content)?;
    }

    // Create symbolic links
    let symlink_tests = [
        // Link to a file
        ("dir1/link_to_test1.txt", "dir1/test1.txt"),
        // Link to a directory
        ("dir2/link_to_subdir1", "dir2/subdir1"),
        // Another link to a file
        ("dir3/link_to_test6.log", "dir3/subdir1/test6.log"),
    ];
    for (link_path, target_path) in symlink_tests.iter() {
        symlink(base_path.join(target_path), base_path.join(link_path))?;
    }

    //-----------------------------------------------------------------------
    // Test cases
    //-----------------------------------------------------------------------
    let test_cases = vec![
        // -----------------------------------------------------------------
        // Original examples (converted to expected_counts = 1 each)
        // -----------------------------------------------------------------
        TestCase {
            pattern: "*.log",
            // Expect .log files only, ignoring symlinks
            expected_counts: vec![
                ("test2.log", 1),
                ("test4.log", 1),
                ("test6.log", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"), // only actual files
            symlink_mode: None,     // default -P
            description: "Basic glob pattern for .log files (regular files only)",
        },
        TestCase {
            pattern: "*.log",
            // Expect logs AND the symlink to test6.log
            expected_counts: vec![
                ("test2.log", 1),
                ("test4.log", 1),
                ("test6.log", 1),
                ("link_to_test6.log", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: None, // default "any"
            symlink_mode: None, 
            description: "Find .log files plus any symlink that ends with .log",
        },
        // Filter by type = f (only files)
        TestCase {
            pattern: "test*",
            expected_counts: vec![
                ("test1.txt", 1),
                ("test2.log", 1),
                ("test3.txt", 1),
                ("test4.log", 1),
                ("test5.txt", 1),
                ("test6.log", 1),
                ("test7.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find only files with 'test*' pattern",
        },
        // Filter by type = d (only dirs)
        TestCase {
            pattern: "sub*",
            expected_counts: vec![
                ("subdir1", 3),
                ("subdir2", 1),
                ("subsubdir1", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("d"),
            symlink_mode: None,
            description: "Find only directories with 'sub*' pattern",
        },
        // Filter by type = l (only symlinks)
        TestCase {
            pattern: "link_*",
            expected_counts: vec![
                ("link_to_test1.txt", 1),
                ("link_to_subdir1", 1),
                ("link_to_test6.log", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("l"),
            symlink_mode: None,
            description: "Find only symbolic links with 'link_*' pattern",
        },
        // Combined pattern + filter
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("test1.txt", 1),
                ("test3.txt", 1),
                ("test5.txt", 1),
                ("test7.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find only .txt files (excluding symlink-to-txt)",
        },
        // Depth limit
        TestCase {
            pattern: "sub*",
            expected_counts: vec![
                ("subdir1", 3),
                ("subdir2", 1),
                ("subsubdir1", 1),
            ],
            max_depth: Some(2),
            threads: Some(1),
            type_filter: Some("d"),
            symlink_mode: None,
            description: "Find only directories with sub* pattern (depth limit)",
        },

        // -----------------------------------------------------------------
        // NEW: tests for symlink modes -H (command line) and -L (always)
        // -----------------------------------------------------------------

        // 1) -L: Always follow symlinks
        // Pattern matches "*test6.log", so it will match "test6.log" (real file)
        // and "link_to_test6.log" (symlink). Since -L follows all symlinks,
        // we expect to see them both.  Also, we see them anyway because
        // the symlink name matches the pattern. But crucially, if there's
        // a second route to the same file, we might see additional duplicates.
        // Here, let's keep it simple: we expect at least these 2 appearances.
        TestCase {
            pattern: "*test6.log",
            expected_counts: vec![
                ("test6.log", 1),
                ("link_to_test6.log", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: None,
            symlink_mode: Some("-L"), // follow all symlinks
            description: "Always follow symlinks with -L; expect link + file for test6.log",
        },

        // 2) -H: Follow symlinks only if they are on the command line
        // In this example, we still pass the 'base_path' as the directory, so
        // these symlinks are *discovered inside the recursion*, not on the CLI.
        // That means for -H mode we do NOT follow them deeper. But we still see
        // them *as symlinks themselves* if they match the pattern. We'll match
        // "*test6.log" -> "link_to_test6.log" and the real "test6.log".
        // So ironically, we will still see 2 lines here (the file and the symlink).
        // If you truly wanted to see the difference vs. -L, you'd create a link
        // to a directory that leads to more files, etc.
        TestCase {
            pattern: "*test6.log",
            expected_counts: vec![
                ("test6.log", 1),
                ("link_to_test6.log", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: None,
            symlink_mode: Some("-H"),
            description: "Follow symlinks only if on command line (-H). Here, they're discovered, so not followed, but still matched as symlinks.",
        },

        // 3) An example to demonstrate that -H *does* follow symlink if used as the CLI dir:
        // If you'd like to see -H in action, you can pass the symlinked directory as the root.
        // For instance, "dir2/link_to_subdir1" is a link to "dir2/subdir1". We'll match "test5.txt".
        // If we specify that symlink path as the root directory, then -H will expand it,
        // whereas default -P wouldn't.
        // This example forces an override of the directory under test.
        TestCase {
            pattern: "test5.txt",
            // We only expect to see "test5.txt" once if -H actually enters that subdir.
            // If we used -P with that symlink dir, we'd see zero results (it wouldn't
            // follow the link).
            expected_counts: vec![
                ("test5.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: None,
            symlink_mode: Some("-H"),
            description: "Follow symlink if it's the command line root (-H). Expect test5.txt once.",
        },
    ];

    // Path to our compiled test binary (e.g. "rfind")
    let mut bin_path = env::current_exe()?;
    bin_path.pop(); // remove "test_file_finder_integration" binary
    bin_path.pop(); // remove "deps"
    bin_path.push("rfind"); // the actual finder binary

    for test_case in test_cases {
        println!("\nRunning test case: {}", test_case.description);
        println!("Pattern: {}", test_case.pattern);

        // Construct the command
        let mut cmd = Command::new(&bin_path);

        cmd.arg(test_case.pattern)
            .arg("--dir")
            .arg({
                // If you want an actual example of calling a symlinked dir as root,
                // you'd do something like:
                //   base_path.join("dir2").join("link_to_subdir1")
                // But for simplicity, we’ll do `base_path` for all except our 3rd symlink test.
                if test_case.description.contains("Follow symlink if it's the command line root") {
                    // Force scanning from the symlinked directory
                    base_path.join("dir2").join("link_to_subdir1")
                } else {
                    // Default to scanning from the base fixture
                    base_path.to_path_buf()
                }
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(depth) = test_case.max_depth {
            cmd.arg("--max-depth").arg(depth.to_string());
        }
        if let Some(threads) = test_case.threads {
            cmd.arg("--threads").arg(threads.to_string());
        }
        if let Some(tfilter) = test_case.type_filter {
            cmd.arg("--type").arg(tfilter);
        }
        if let Some(symlink_flag) = test_case.symlink_mode {
            // e.g. -H or -L
            cmd.arg(symlink_flag);
        }

        // Spawn the child process
        let mut child = cmd.spawn()?;

        // We'll store each discovered filename in a Map<filename, count>.
        let mut found_counts: HashMap<String, usize> = HashMap::new();

        // Read stdout line by line
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                let line = line_result?;
                // Trim it and parse out the file_name
                if let Some(file_name) = Path::new(line.trim()).file_name().and_then(|n| n.to_str())
                {
                    *found_counts.entry(file_name.to_string()).or_insert(0) += 1;
                }
            }
        }

        // Wait for the process
        let status = child.wait()?;
        assert!(status.success(), "Process failed with status: {}", status);

        // Compare found_counts to expected_counts
        let expected_map = make_expected_map(&test_case.expected_counts);

        println!("  Expected: {:?}", expected_map);
        println!("  Found:    {:?}", found_counts);

        // Check each expected item
        for (expected_file, &expected_count) in &expected_map {
            let actual_count = found_counts.get(expected_file).copied().unwrap_or(0);
            assert_eq!(
                actual_count, expected_count,
                "Mismatch in counts for '{}': expected {}, found {}",
                expected_file, expected_count, actual_count
            );
        }

        // Also check for any extra items we did not expect
        for (found_file, &count) in &found_counts {
            if !expected_map.contains_key(found_file) && count > 0 {
                panic!(
                    "Found unexpected file '{}' with count {} in test '{}'",
                    found_file, count, test_case.description
                );
            }
        }
    }

    Ok(())
}
