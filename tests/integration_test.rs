use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::io::{BufRead, BufReader};
use tempfile::TempDir;
use std::collections::HashSet;
use std::env;

#[test]
fn test_file_finder_integration() -> Result<(), Box<dyn std::error::Error>> {
    // Create a temporary directory structure for testing
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path();

    // Create test directory structure
    let test_dirs = [
        "dir1/subdir1",
        "dir1/subdir2",
        "dir2/subdir1",
        "dir3/subdir1/subsubdir1",
    ];

    // Create test files
    let test_files = [
        ("dir1/test1.txt", "content1"),
        ("dir1/subdir1/test2.log", "content2"),
        ("dir1/subdir2/test3.txt", "content3"),
        ("dir2/test4.log", "content4"),
        ("dir2/subdir1/test5.txt", "content5"),
        ("dir3/subdir1/test6.log", "content6"),
        ("dir3/subdir1/subsubdir1/test7.txt", "content7"),
    ];

    // Create directories
    for dir in test_dirs.iter() {
        fs::create_dir_all(base_path.join(dir))?;
    }

    // Create files
    for (path, content) in test_files.iter() {
        let file_path = base_path.join(path);
        fs::write(file_path, content)?;
    }

    // Test cases
    let test_cases = vec![
        TestCase {
            pattern: "*.log",
            expected_files: vec!["test2.log", "test4.log", "test6.log"].into_iter().map(String::from).collect(),
            max_depth: None,
            threads: Some(1),
        },
        TestCase {
            pattern: "test",
            expected_files: vec![
                "test1.txt", "test2.log", "test3.txt", "test4.log",
                "test5.txt", "test6.log", "test7.txt"
            ].into_iter().map(String::from).collect(),
            max_depth: None,
            threads: Some(1),
        },
        TestCase {
            pattern: "*.txt",
            expected_files: vec!["test1.txt", "test3.txt", "test5.txt"].into_iter().map(String::from).collect(),
            max_depth: Some(2),
            threads: Some(1),
        },
    ];

    // Get the path to the compiled binary
    let mut bin_path = env::current_exe()?;
    bin_path.pop(); // Remove the test executable name
    bin_path.pop(); // Remove 'deps' directory
    bin_path.push("rfind"); // Add the actual binary name

    // Run test cases
    for test_case in test_cases {
        println!("\nRunning test case for pattern: {}", test_case.pattern);
        
        let mut cmd = Command::new(&bin_path);
        
        cmd.arg(&test_case.pattern)
           .arg("--dir")
           .arg(base_path)
           .stdout(Stdio::piped())
           .stderr(Stdio::piped());

        if let Some(depth) = test_case.max_depth {
            cmd.arg("--max-depth")
               .arg(depth.to_string());
        }

        if let Some(threads) = test_case.threads {
            cmd.arg("--threads")
               .arg(threads.to_string());
        }

        // Run the command
        let mut child = cmd.spawn()?;
        
        // Create a set to store found files
        let mut found_files = HashSet::new();
        
        // Read stdout line by line
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let line = line?;
                if !line.contains("Total matches found:") && 
                   !line.contains("Used") && 
                   !line.contains("Total time:") {
                    if let Some(file_name) = Path::new(&line.trim())
                        .file_name()
                        .and_then(|n| n.to_str()) {
                        found_files.insert(String::from(file_name));
                    }
                }
            }
        }

        // Wait for the process to complete
        let status = child.wait()?;
        assert!(status.success(), "Process failed with status: {}", status);

        // Print debug information
        println!("Expected files: {:?}", test_case.expected_files);
        println!("Found files: {:?}", found_files);

        // Check for missing files
        let missing_files: HashSet<_> = test_case.expected_files
            .difference(&found_files)
            .collect();
        
        // Check for unexpected files
        let unexpected_files: HashSet<_> = found_files
            .difference(&test_case.expected_files)
            .collect();

        assert!(
            missing_files.is_empty() && unexpected_files.is_empty(),
            "File mismatch for pattern '{}'\nMissing files: {:?}\nUnexpected files: {:?}",
            test_case.pattern,
            missing_files,
            unexpected_files
        );
    }

    Ok(())
}

struct TestCase {
    pattern: &'static str,
    expected_files: HashSet<String>,
    max_depth: Option<usize>,
    threads: Option<usize>,
}