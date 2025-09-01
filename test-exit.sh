#!/bin/bash

echo "Starting test script..."

# Simple cleanup function
cleanup() {
    echo "Cleanup called"
}

trap cleanup EXIT

echo "Script completed successfully"
echo "Exit code should be 0"
