#!/usr/bin/env bash

# Exit immediately if a command exits with a non-zero status.
set -e

# Where to create the test directory structure.
# Adjust "test_root" to any path you want.
TEST_ROOT="test"

# Clean up if TEST_ROOT already exists (Optional)
rm -rf "$TEST_ROOT"

# 1. Create directories
mkdir -p "$TEST_ROOT/dir1/subdir1"
mkdir -p "$TEST_ROOT/dir1/subdir2"
mkdir -p "$TEST_ROOT/dir2/subdir1"
mkdir -p "$TEST_ROOT/dir3/subdir1/subsubdir1"

# 2. Create files with given content
echo "content1" > "$TEST_ROOT/dir1/test1.txt"
echo "content2" > "$TEST_ROOT/dir1/subdir1/test2.log"
echo "content3" > "$TEST_ROOT/dir1/subdir2/test3.txt"
echo "content4" > "$TEST_ROOT/dir2/test4.log"
echo "content5" > "$TEST_ROOT/dir2/subdir1/test5.txt"
echo "content6" > "$TEST_ROOT/dir3/subdir1/test6.log"
echo "content7" > "$TEST_ROOT/dir3/subdir1/subsubdir1/test7.txt"

# 3. Create symlinks
#    - The target paths here are relative so that the symlinks
#      behave correctly within this local tree structure.

# Regular symlinks to files
ln -s "test1.txt"           "$TEST_ROOT/dir1/link_to_test1.txt"
ln -s "test4.log"           "$TEST_ROOT/dir2/link_to_test4.log"

# Symlinks to directories
ln -s "subdir1"             "$TEST_ROOT/dir1/link_to_subdir1"
ln -s "../dir3"             "$TEST_ROOT/dir2/link_to_dir3"

# Broken symlink
ln -s "nonexistent.txt"     "$TEST_ROOT/dir1/broken_link.txt"

# Nested symlink (symlink to another symlink)
ln -s "../dir1/link_to_subdir1" "$TEST_ROOT/dir2/nested_link"

# Symlink loop
ln -s "loop2"               "$TEST_ROOT/dir3/loop1"
ln -s "loop1"               "$TEST_ROOT/dir3/loop2"

echo "Directory structure created under '$TEST_ROOT'"

# Example usage of 'find' to see the structure:
# echo
# echo "Example: find all .txt files (not following symlinks by default):"
# find "$TEST_ROOT" -name '*.txt'

# echo
# echo "Example: find all .log files following symlinks (-L):"
# find -L "$TEST_ROOT" -name '*.log'
