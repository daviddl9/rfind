#!/bin/bash

# Number of times to run each command
NUM_RUNS=5
# Directory to search in
SEARCH_DIR="/usr"
# Pattern to search for
PATTERN="*.log"

# Function to get time in seconds with millisecond precision
get_time_ms() {
    echo $(($(date +%s%N)/1000000))
}

# Function to run a command multiple times and calculate average
benchmark_command() {
    local command="$1"
    local name="$2"
    local total_time=0
    
    echo "Running $name benchmark..."
    echo "------------------------"
    
    for i in $(seq 1 $NUM_RUNS); do
        echo "Run $i:"
        
        # Get start time
        start=$(get_time_ms)
        
        # Run the command and discard output
        eval "$command" > /dev/null 2>&1
        
        # Get end time
        end=$(get_time_ms)
        
        # Calculate duration in seconds
        duration=$(echo "scale=3; ($end - $start) / 1000" | bc)
        
        echo "  Time taken: ${duration}s"
        echo "------------------------"
        
        total_time=$(echo "$total_time + $duration" | bc)
    done
    
    # Calculate average
    average_time=$(echo "scale=3; $total_time / $NUM_RUNS" | bc)
    echo "Average time for $name: ${average_time}s"
    echo ""
    
    # Return the average time
    echo "$average_time"
}

# Ensure the rfind binary is built with release optimizations
echo "Building rfind in release mode..."
cargo build --release

echo "Starting benchmarks..."
echo "Search directory: $SEARCH_DIR"
echo "Pattern: $PATTERN"
echo "Number of runs: $NUM_RUNS"
echo ""

# Run traditional find
find_avg=$(benchmark_command "find \"$SEARCH_DIR\" -name \"$PATTERN\"" "find")

# Run our rfind implementation
rfind_avg=$(benchmark_command "./target/release/rfind \"$PATTERN\" -d \"$SEARCH_DIR\"" "rfind")

# Calculate speedup
speedup=$(echo "scale=2; $find_avg / $rfind_avg" | bc)

echo "Summary:"
echo "------------------------"
echo "find average: ${find_avg}s"
echo "rfind average: ${rfind_avg}s"
echo "Speedup: ${speedup}x"

# Save results to a CSV file
echo "timestamp,pattern,search_dir,find_time,rfind_time,speedup" > benchmark_results.csv
echo "$(date '+%Y-%m-%d %H:%M:%S'),$PATTERN,$SEARCH_DIR,$find_avg,$rfind_avg,$speedup" >> benchmark_results.csv