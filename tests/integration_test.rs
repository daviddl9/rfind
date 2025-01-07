use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::TempDir;
use filetime::*;
use std::time::{SystemTime, Duration};
use rfind::permissions::{PermissionFilter, PermissionMode, PermissionType, SpecialMode, has_special_mode};
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, MetadataExt};
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use tempfile::tempdir;

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
    /// If present, this path (relative to the `base_path`) will be used
    /// for `--dir`; otherwise we use the default top-level fixture directory.
    base_path_override: Option<&'static str>,
    mtime: Option<&'static str>,
    atime: Option<&'static str>,
    ctime: Option<&'static str>,
    size: Option<&'static str>,
    perm: Option<&'static str>,
    gid: Option<&'static str>,
    uid: Option<&'static str>,
}

/// Helper struct to manage test file timestamps
struct TimeTestFile {
    path: String,
    content: &'static str,
    /// Offset in minutes from test start time
    mtime_offset: i64,
    atime_offset: i64,
}

#[test]
fn test_file_finder_size_filters() -> Result<(), Box<dyn std::error::Error>> {
    // Create a temporary directory structure for testing
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path();

    // Create test directory
    fs::create_dir_all(base_path.join("size_test"))?;
    
    // Pre-create the repeated strings
    let small_content = "a".repeat(1024);           // 1KB
    let medium_content = "b".repeat(1024 * 100);    // 100KB
    let large_content = "c".repeat(1024 * 1024);    // 1MB
    let huge_content = "d".repeat(1024 * 1024 * 5); // 5MB

    // Create files of different sizes
    let test_files = vec![
        ("size_test/empty.txt", ""),                // 0 bytes
        ("size_test/tiny.txt", "small"),           // 5 bytes
        ("size_test/small.txt", &small_content),   // 1KB
        ("size_test/medium.txt", &medium_content), // 100KB
        ("size_test/large.txt", &large_content),   // 1MB
        ("size_test/huge.txt", &huge_content),     // 5MB
    ];

    // Create the test files
    for (path, content) in &test_files {
        let file_path = base_path.join(path);
        fs::write(&file_path, content)?;
        
        // Debug: Print actual file sizes
        let metadata = fs::metadata(&file_path)?;
        println!("File: {} (size: {} bytes)", path, metadata.len());
    }

    // Size-based test cases
    let size_test_cases = vec![
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("tiny.txt", 1),
                ("empty.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files smaller than 10 bytes",
            base_path_override: Some("size_test"),
            size: Some("-10c"),     // Less than 10 bytes
            mtime: None,
            atime: None,
            ctime: None,
            perm: None,
            gid: None,
            uid: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("medium.txt", 1),
                ("large.txt", 1),
                ("huge.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files larger than 50KB",
            base_path_override: Some("size_test"),
            size: Some("+50k"),     // Larger than 50KB
            mtime: None,
            atime: None,
            ctime: None,
            perm: None,
            gid: None,
            uid: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("large.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files exactly 1MB in size",
            base_path_override: Some("size_test"),
            size: Some("1M"),       // Exactly 1MB
            mtime: None,
            atime: None,
            ctime: None,
            perm: None,
            gid: None,
            uid: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("huge.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files larger than 2MB",
            base_path_override: Some("size_test"),
            size: Some("+2M"),      // Larger than 2MB
            mtime: None,
            atime: None,
            ctime: None,
            perm: None,
            gid: None,
            uid: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("small.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files exactly 1KB in size",
            base_path_override: Some("size_test"),
            size: Some("1k"),       // Exactly 1KB
            mtime: None,
            atime: None,
            ctime: None,
            perm: None,
            gid: None,
            uid: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("empty.txt", 1),
                ("tiny.txt", 1),
                ("small.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files smaller than 2KB",
            base_path_override: Some("size_test"),
            size: Some("-2k"),      // Smaller than 2KB
            mtime: None,
            atime: None,
            ctime: None,
            perm: None,
            gid: None,
            uid: None,
        },
    ];

    // Path to our compiled test binary
    let mut bin_path = env::current_exe()?;
    bin_path.pop(); // remove test binary name
    bin_path.pop(); // remove "deps"
    bin_path.push("rfind");

    // Execute each test case
    for test_case in size_test_cases {
        println!("\nRunning size filter test case: {}", test_case.description);
        println!("Pattern: {}", test_case.pattern);

        // Build command
        let mut cmd = Command::new(&bin_path);
        
        let base_dir = if let Some(rel_path) = test_case.base_path_override {
            base_path.join(rel_path)
        } else {
            base_path.to_path_buf()
        };

        // Basic arguments
        cmd.arg(test_case.pattern)
            .arg("--dir")
            .arg(&base_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Optional arguments
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
            cmd.arg(symlink_flag);
        }
        if let Some(size) = test_case.size {
            cmd.arg("--size").arg(size);
            println!("  With size filter: {}", size);
        }

        // Run command and collect results
        let mut child = cmd.spawn()?;
        let mut found_counts: HashMap<String, usize> = HashMap::new();

        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                let line = line_result?;
                if let Some(file_name) = Path::new(line.trim()).file_name().and_then(|n| n.to_str()) {
                    *found_counts.entry(file_name.to_string()).or_insert(0) += 1;
                }
            }
        }

        // Check process status
        let status = child.wait()?;
        if !status.success() {
            let mut error_message = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                std::io::Read::read_to_string(&mut stderr, &mut error_message)?;
            }
            return Err(format!(
                "Process failed in test '{}' with status: {}. Stderr: {}",
                test_case.description, status, error_message
            ).into());
        }

        // Verify results
        let expected_map = make_expected_map(&test_case.expected_counts);
        println!("  Expected counts: {:?}", expected_map);
        println!("  Found counts:    {:?}", found_counts);

        // Check for expected files
        for (expected_file, &expected_count) in &expected_map {
            let actual_count = found_counts.get(expected_file).copied().unwrap_or(0);
            assert_eq!(
                actual_count, expected_count,
                "Test '{}': Mismatch for file '{}' - expected {} occurrences, found {}",
                test_case.description, expected_file, expected_count, actual_count
            );
        }

        // Check for unexpected files
        for (found_file, &count) in &found_counts {
            if !expected_map.contains_key(found_file.as_str()) && count > 0 {
                return Err(format!(
                    "Test '{}': Found unexpected file '{}' with count {}",
                    test_case.description, found_file, count
                ).into());
            }
        }

        println!("  ✓ Test passed: {}", test_case.description);
    }

    Ok(())
}

#[test]
fn test_permission_filter_parsing() {
    let filter = PermissionFilter::parse("u+x").unwrap();
    assert!(matches!(filter.mode, PermissionMode::User));
    assert!(matches!(filter.perm_type, PermissionType::Execute));
    assert!(filter.expected);

    assert!(PermissionFilter::parse("invalid").is_err());
}

#[cfg(unix)]
#[test]
fn test_permission_matching() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.txt");
    let file = File::create(&file_path).unwrap();
    
    // Set permission to 755 (rwxr-xr-x)
    file.set_permissions(std::fs::Permissions::from_mode(0o755)).unwrap();
    
    let metadata = std::fs::metadata(&file_path).unwrap();
    
    let filter = PermissionFilter::parse("u+x").unwrap();
    assert!(filter.matches(&metadata));
    
    let filter = PermissionFilter::parse("o-w").unwrap();
    assert!(filter.matches(&metadata));
}

#[cfg(unix)]
#[test]
fn test_special_modes() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.txt");
    let file = File::create(&file_path).unwrap();
    
    // Set setuid bit (4755)
    file.set_permissions(std::fs::Permissions::from_mode(0o4755)).unwrap();
    
    let metadata = std::fs::metadata(&file_path).unwrap();
    assert!(has_special_mode(&metadata, SpecialMode::SetUID));
    assert!(!has_special_mode(&metadata, SpecialMode::SetGID));
}

#[test]
fn test_file_finder_time_filters() -> Result<(), Box<dyn std::error::Error>> {
    // Create a temporary directory structure for testing
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path();

    // Create test directories
    fs::create_dir_all(base_path.join("time_test"))?;
    
    // Use current time as base
    let now = SystemTime::now();
    
    // Define test files with specific timestamps relative to now
    let test_files = vec![
        TimeTestFile {
            path: "time_test/recent.txt".into(),
            content: "recent file",
            mtime_offset: -5,     // 5 minutes ago (should match -10m)
            atime_offset: -3,     // 3 minutes ago
        },
        TimeTestFile {
            path: "time_test/hour_old.txt".into(),
            content: "hour old file",
            mtime_offset: -60,    // 1 hour ago
            atime_offset: -30,    // 30 minutes ago
        },
        TimeTestFile {
            path: "time_test/day_old.txt".into(),
            content: "old file",
            mtime_offset: -180,   // 3 hours ago
            atime_offset: -120,   // 2 hours ago
        },
    ];

    // Create files and set their timestamps
    for file in &test_files {
        let file_path = base_path.join(&file.path);
        fs::write(&file_path, file.content)?;
        
        // Calculate timestamp relative to now
        let mtime = now - Duration::from_secs(file.mtime_offset.unsigned_abs() * 60);
        let atime = now - Duration::from_secs(file.atime_offset.unsigned_abs() * 60);
        
        filetime::set_file_times(
            &file_path,
            FileTime::from_system_time(atime),
            FileTime::from_system_time(mtime),
        )?;

        // Debug: Print actual timestamps and their ages
        let metadata = fs::metadata(&file_path)?;
        let actual_mtime = metadata.modified()?;
        let age = now.duration_since(actual_mtime)
            .map(|d| format!("{:.0} minutes", d.as_secs() as f64 / 60.0))
            .unwrap_or_else(|_| "error".to_string());
        
        println!("File: {} (age: {})", file.path, age);
    }

    // Path to our compiled test binary
    let mut bin_path = env::current_exe()?;
    bin_path.pop(); // remove test binary name
    bin_path.pop(); // remove "deps"
    bin_path.push("rfind");

    // Time-based test cases
    let time_test_cases = vec![
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("recent.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            mtime: Some("-10m"),    // Less than 10 minutes old
            atime: None,
            ctime: None,
            description: "Find files modified less than 10 minutes ago",
            base_path_override: Some("time_test"),
            size: None,
            perm: None,
            gid: None,
            uid: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("hour_old.txt", 1),
                ("day_old.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            mtime: Some("+30m"),    // More than 30 minutes old
            atime: None,
            ctime: None,
            description: "Find files modified more than 30 minutes ago",
            base_path_override: Some("time_test"),
            size: None,
            perm: None,
            gid: None,
            uid: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("hour_old.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            mtime: Some("1h"),      // Exactly 1 hour old (within 1-minute margin)
            atime: None,
            ctime: None,
            description: "Find files modified exactly 1 hour ago",
            base_path_override: Some("time_test"),
            size: None,
            perm: None,
            gid: None,
            uid: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("recent.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            mtime: None,
            atime: Some("-5m"),     // Accessed less than 5 minutes ago
            ctime: None,
            description: "Find files accessed less than 5 minutes ago",
            base_path_override: Some("time_test"),
            size: None,
            perm: None,
            gid: None,
            uid: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("hour_old.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            mtime: Some("+30m"),    // Modified more than 30 minutes ago
            atime: Some("-60m"),    // Accessed less than 60 minutes ago
            ctime: None,
            description: "Find files with combined modification and access time filters",
            base_path_override: Some("time_test"),
            size: None,
            perm: None,
            gid: None,
            uid: None,
        },
        #[cfg(unix)]
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("recent.txt", 1),
                ("hour_old.txt", 1),
                ("day_old.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            mtime: None,
            atime: None,
            ctime: Some("-120m"),   // Changed less than 2 hours ago
            description: "Find files changed less than 2 hours ago (Unix only)",
            base_path_override: Some("time_test"),
            size: None,
            perm: None,
            gid: None,
            uid: None,
        },
    ];

    // Execute each test case
    for test_case in time_test_cases {
        println!("\nRunning time filter test case: {}", test_case.description);
        println!("Pattern: {}", test_case.pattern);

        // Build command
        let mut cmd = Command::new(&bin_path);
        
        let base_dir = if let Some(rel_path) = test_case.base_path_override {
            base_path.join(rel_path)
        } else {
            base_path.to_path_buf()
        };

        // Basic arguments
        cmd.arg(test_case.pattern)
            .arg("--dir")
            .arg(&base_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Optional arguments
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
            cmd.arg(symlink_flag);
        }

        // Time-based filters
        if let Some(mtime) = test_case.mtime {
            cmd.arg("--mtime").arg(mtime);
            println!("  With mtime filter: {}", mtime);
        }
        if let Some(atime) = test_case.atime {
            cmd.arg("--atime").arg(atime);
            println!("  With atime filter: {}", atime);
        }
        if let Some(ctime) = test_case.ctime {
            cmd.arg("--ctime").arg(ctime);
            println!("  With ctime filter: {}", ctime);
        }

        // Run command and collect results
        let mut child = cmd.spawn()?;
        let mut found_counts: HashMap<String, usize> = HashMap::new();

        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                let line = line_result?;
                if let Some(file_name) = Path::new(line.trim()).file_name().and_then(|n| n.to_str()) {
                    *found_counts.entry(file_name.to_string()).or_insert(0) += 1;
                }
            }
        }

        // Check process status
        let status = child.wait()?;
        if !status.success() {
            let mut error_message = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                std::io::Read::read_to_string(&mut stderr, &mut error_message)?;
            }
            return Err(format!(
                "Process failed in test '{}' with status: {}. Stderr: {}",
                test_case.description, status, error_message
            ).into());
        }

        // Verify results
        let expected_map = make_expected_map(&test_case.expected_counts);
        println!("  Expected counts: {:?}", expected_map);
        println!("  Found counts:    {:?}", found_counts);

        // Check for expected files
        for (expected_file, &expected_count) in &expected_map {
            let actual_count = found_counts.get(expected_file).copied().unwrap_or(0);
            assert_eq!(
                actual_count, expected_count,
                "Test '{}': Mismatch for file '{}' - expected {} occurrences, found {}",
                test_case.description, expected_file, expected_count, actual_count
            );
        }

        // Check for unexpected files
        for (found_file, &count) in &found_counts {
            if !expected_map.contains_key(found_file.as_str()) && count > 0 {
                return Err(format!(
                    "Test '{}': Found unexpected file '{}' with count {}",
                    test_case.description, found_file, count
                ).into());
            }
        }

        println!("  ✓ Test passed: {}", test_case.description);
    }

    Ok(())
}

/// Convert a slice of (file_name, count) into a HashMap.
fn make_expected_map(items: &[(&str, usize)]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for (name, count) in items {
        map.insert(name.to_string(), *count);
    }
    map
}

#[cfg(windows)]
fn create_symlink(target: impl AsRef<Path>, link: impl AsRef<Path>, is_dir: bool) -> std::io::Result<()> {
    if is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

#[cfg(unix)]
fn create_symlink(target: impl AsRef<Path>, link: impl AsRef<Path>, _is_dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
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
        ("dir1/link_to_test1.txt", "dir1/test1.txt", false),
        // Link to a directory
        ("dir2/link_to_subdir1", "dir2/subdir1", true),
        // Another link to a file
        ("dir3/link_to_test6.log", "dir3/subdir1/test6.log", false),
    ];
    for (link_path, target_path, is_dir) in symlink_tests.iter() {
        create_symlink(base_path.join(target_path), base_path.join(link_path), *is_dir)?;
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
            base_path_override: None,
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
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
            base_path_override: None,
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
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
            base_path_override: None,
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
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
            base_path_override: None,
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
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
            base_path_override: None,
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
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
            base_path_override: None,
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
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
            base_path_override: None,
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
        },
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
            base_path_override: None,
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
        },

        // 2) -H: Follow symlinks only if they are on the command line
        // In this example, we still pass the 'base_path' as the directory, so
        // these symlinks are *discovered inside the recursion*, not on the CLI.
        // That means for -H mode we do NOT follow them deeper. But we still see
        // them *as symlinks themselves* if they match the pattern. We'll match
        // "*test6.log" -> "link_to_test6.log" and the real "test6.log".
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
            base_path_override: None,
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
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
            base_path_override: Some("dir2/link_to_subdir1"),
            atime: None,
            ctime: None,
            mtime: None,
            size: None,
            perm: None,
            gid: None,
            uid: None,
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

        // Decide which directory to scan:
        // if base_path_override is present, join it to base_path.
        // Otherwise, use the default base_path from the TempDir.
        let base_dir = if let Some(rel_path) = test_case.base_path_override {
            base_path.join(rel_path)
        } else {
            base_path.to_path_buf()
        };

        cmd.arg(test_case.pattern)
            .arg("--dir")
            .arg(base_dir)
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

#[cfg(unix)]
fn set_file_permissions(file_path: &Path, mode: u32) -> std::io::Result<()> {
    fs::set_permissions(file_path, fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
fn set_file_permissions(_file_path: &Path, _mode: u32) -> std::io::Result<()> {
    // On Windows, just succeed without doing anything
    Ok(())
}

// Let's modify the permission test functions
#[cfg(unix)]
fn print_file_mode(path: &str, metadata: &fs::Metadata) {
    println!("File: {} (mode: {:o})", path, metadata.mode() & 0o7777);
}

#[cfg(windows)]
fn print_file_mode(path: &str, metadata: &fs::Metadata) {
    println!("File: {} (attributes: {:x})", path, metadata.file_attributes());
}

#[cfg(unix)]
#[test]
fn test_file_finder_permission_filters() -> Result<(), Box<dyn std::error::Error>> {
    // Create a temporary directory structure for testing
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path();

    // Create test directory
    fs::create_dir_all(base_path.join("perm_test"))?;
    
    // Create test files with different permissions
    let test_files = vec![
        ("perm_test/exec.txt", 0o755),      // rwxr-xr-x
        ("perm_test/no_exec.txt", 0o644),   // rw-r--r--
        ("perm_test/all_exec.txt", 0o777),  // rwxrwxrwx
        ("perm_test/no_read.txt", 0o333),   // -wx-wx-wx
        ("perm_test/no_write.txt", 0o555),  // r-xr-xr-x
        ("perm_test/group_write.txt", 0o674),// rw-rwxr--
        ("perm_test/setuid.txt", 0o4755),   // rwsr-xr-x
        ("perm_test/setgid.txt", 0o2755),   // rwxr-sr-x
        ("perm_test/sticky.txt", 0o1755),   // rwxr-xr-t
    ];

    // Create the test files with specific permissions
    for (path, mode) in &test_files {
        let file_path = base_path.join(path);
        File::create(&file_path)?;
        set_file_permissions(&file_path, *mode)?;
        
        // Debug: Print actual permissions
        let metadata = fs::metadata(&file_path)?;
        print_file_mode(path, &metadata);
    }

    // Permission-based test cases
    let perm_test_cases = vec![
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("exec.txt", 1),
                ("all_exec.txt", 1),
                ("no_write.txt", 1),
                ("setuid.txt", 1),
                ("setgid.txt", 1),
                ("sticky.txt", 1),
                ("no_read.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files with user execute permission",
            base_path_override: Some("perm_test"),
            perm: Some("u+x"),
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
            size: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("no_exec.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files without group execute permission",
            base_path_override: Some("perm_test"),
            perm: Some("g-x"),
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
            size: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("group_write.txt", 1),
                ("all_exec.txt", 1),
                ("no_read.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files with group write permission",
            base_path_override: Some("perm_test"),
            perm: Some("g+w"),
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
            size: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("no_read.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files without read permission for others",
            base_path_override: Some("perm_test"),
            perm: Some("o-r"),
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
            size: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("all_exec.txt", 1),
                ("no_read.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find files with write permission for all",
            base_path_override: Some("perm_test"),
            perm: Some("a+w"),
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
            size: None,
        },
        // Test for setuid bit
        TestCase {
            pattern: "setuid.txt",
            expected_counts: vec![
                ("setuid.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find file with setuid bit",
            base_path_override: Some("perm_test"),
            perm: Some("u+s"),
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
            size: None,
        },
        // Test for setgid bit
        TestCase {
            pattern: "setgid.txt",
            expected_counts: vec![
                ("setgid.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find file with setgid bit",
            base_path_override: Some("perm_test"),
            perm: Some("g+s"),
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
            size: None,
        },
    ];

    // Path to our compiled test binary
    let mut bin_path = env::current_exe()?;
    bin_path.pop(); // remove test binary name
    bin_path.pop(); // remove "deps"
    bin_path.push("rfind");

    // Execute each test case
    for test_case in perm_test_cases {
        println!("\nRunning permission filter test case: {}", test_case.description);
        println!("Pattern: {}", test_case.pattern);

        // Build command
        let mut cmd = Command::new(&bin_path);
        
        let base_dir = if let Some(rel_path) = test_case.base_path_override {
            base_path.join(rel_path)
        } else {
            base_path.to_path_buf()
        };

        // Basic arguments
        cmd.arg(test_case.pattern)
            .arg("--dir")
            .arg(&base_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Optional arguments
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
            cmd.arg(symlink_flag);
        }
        if let Some(perm) = test_case.perm {
            cmd.arg("--perm").arg(perm);
            println!("  With permission filter: {}", perm);
        }
        if let Some(uid) = test_case.uid {
            cmd.arg("--uid").arg(uid);
        }
        if let Some(gid) = test_case.gid {
            cmd.arg("--gid").arg(gid);
        }

        // Run command and collect results
        let mut child = cmd.spawn()?;
        let mut found_counts: HashMap<String, usize> = HashMap::new();

        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                let line = line_result?;
                if let Some(file_name) = Path::new(line.trim()).file_name().and_then(|n| n.to_str()) {
                    *found_counts.entry(file_name.to_string()).or_insert(0) += 1;
                }
            }
        }

        // Check process status
        let status = child.wait()?;
        if !status.success() {
            let mut error_message = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                std::io::Read::read_to_string(&mut stderr, &mut error_message)?;
            }
            return Err(format!(
                "Process failed in test '{}' with status: {}. Stderr: {}",
                test_case.description, status, error_message
            ).into());
        }

        // Verify results
        let expected_map = make_expected_map(&test_case.expected_counts);
        println!("  Expected counts: {:?}", expected_map);
        println!("  Found counts:    {:?}", found_counts);

        // Check for expected files
        for (expected_file, &expected_count) in &expected_map {
            let actual_count = found_counts.get(expected_file).copied().unwrap_or(0);
            assert_eq!(
                actual_count, expected_count,
                "Test '{}': Mismatch for file '{}' - expected {} occurrences, found {}",
                test_case.description, expected_file, expected_count, actual_count
            );
        }

        // Check for unexpected files
        for (found_file, &count) in &found_counts {
            if !expected_map.contains_key(found_file.as_str()) && count > 0 {
                return Err(format!(
                    "Test '{}': Found unexpected file '{}' with count {}",
                    test_case.description, found_file, count
                ).into());
            }
        }

        println!("  ✓ Test passed: {}", test_case.description);
    }

    Ok(())
}

#[cfg(windows)]
#[test]
fn test_file_finder_permission_filters() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::windows::fs::MetadataExt;
    
    // Create a temporary directory structure for testing
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path();

    // Create test directory
    fs::create_dir_all(base_path.join("perm_test"))?;
    
    // Define test files with their attributes
    struct TestFile {
        name: &'static str,
        attributes: u32,
    }

    // FILE_ATTRIBUTE constants
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;

    let test_files = vec![
        TestFile {
            name: "perm_test/readonly.txt",
            attributes: FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_ARCHIVE,
        },
        TestFile {
            name: "perm_test/writable.txt",
            attributes: FILE_ATTRIBUTE_ARCHIVE,
        },
        TestFile {
            name: "perm_test/hidden.txt",
            attributes: FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_ARCHIVE,
        },
        TestFile {
            name: "perm_test/system.txt",
            attributes: FILE_ATTRIBUTE_SYSTEM | FILE_ATTRIBUTE_ARCHIVE,
        },
    ];

    // Create the test files and set their attributes
    for test_file in &test_files {
        let file_path = base_path.join(test_file.name);
        
        // Create the file first
        File::create(&file_path)?;
        
        // Convert path to wide string for Windows API
        let path_wide: Vec<u16> = file_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
            
        // Set the Windows file attributes
        unsafe {
            winapi::um::fileapi::SetFileAttributesW(
                path_wide.as_ptr(),
                test_file.attributes
            );
        }
        
        // Debug: Print actual attributes
        let metadata = fs::metadata(&file_path)?;
        println!("File: {} (attributes: 0x{:x})", test_file.name, metadata.file_attributes());
    }

    // Windows-specific test cases
    let perm_test_cases = vec![
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("readonly.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find readonly files",
            base_path_override: Some("perm_test"),
            perm: Some("u-w"),  // Maps to readonly on Windows
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
            size: None,
        },
        TestCase {
            pattern: "*.txt",
            expected_counts: vec![
                ("writable.txt", 1),
            ],
            max_depth: None,
            threads: Some(1),
            type_filter: Some("f"),
            symlink_mode: None,
            description: "Find writable files",
            base_path_override: Some("perm_test"),
            perm: Some("u+w"),  // Maps to !readonly on Windows
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
            size: None,
        },
    ];

    // Path to our compiled test binary
    let mut bin_path = env::current_exe()?;
    bin_path.pop(); // remove test binary name
    bin_path.pop(); // remove "deps"
    bin_path.push("rfind");

    // Execute each test case
    for test_case in perm_test_cases {
        println!("\nRunning permission filter test case: {}", test_case.description);
        println!("Pattern: {}", test_case.pattern);

        // Build command
        let mut cmd = Command::new(&bin_path);
        
        let base_dir = if let Some(rel_path) = test_case.base_path_override {
            base_path.join(rel_path)
        } else {
            base_path.to_path_buf()
        };

        // Basic arguments
        cmd.arg(test_case.pattern)
            .arg("--dir")
            .arg(&base_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Optional arguments
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
            cmd.arg(symlink_flag);
        }
        if let Some(perm) = test_case.perm {
            cmd.arg("--perm").arg(perm);
            println!("  With permission filter: {}", perm);
        }

        // Run command and collect results
        let mut child = cmd.spawn()?;
        let mut found_counts: HashMap<String, usize> = HashMap::new();

        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                let line = line_result?;
                if let Some(file_name) = Path::new(line.trim()).file_name().and_then(|n| n.to_str()) {
                    *found_counts.entry(file_name.to_string()).or_insert(0) += 1;
                }
            }
        }

        // Check process status and verify results
        let status = child.wait()?;
        if !status.success() {
            let mut error_message = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                std::io::Read::read_to_string(&mut stderr, &mut error_message)?;
            }
            return Err(format!(
                "Process failed in test '{}' with status: {}. Stderr: {}",
                test_case.description, status, error_message
            ).into());
        }

        // Verify results
        let expected_map = make_expected_map(&test_case.expected_counts);
        println!("  Expected counts: {:?}", expected_map);
        println!("  Found counts:    {:?}", found_counts);

        // Check results match expectations
        for (expected_file, &expected_count) in &expected_map {
            let actual_count = found_counts.get(expected_file).copied().unwrap_or(0);
            assert_eq!(
                actual_count, expected_count,
                "Test '{}': Mismatch for file '{}' - expected {} occurrences, found {}",
                test_case.description, expected_file, expected_count, actual_count
            );
        }

        for (found_file, &count) in &found_counts {
            if !expected_map.contains_key(found_file.as_str()) && count > 0 {
                return Err(format!(
                    "Test '{}': Found unexpected file '{}' with count {}",
                    test_case.description, found_file, count
                ).into());
            }
        }

        println!("  ✓ Test passed: {}", test_case.description);
    }

    Ok(())
}
