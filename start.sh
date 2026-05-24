#!/bin/bash

# Exit on Ctrl+C cleanly and stop child processes
trap "kill 0" EXIT

echo "🚀 Starting Rust Code Studio (macOS/Linux)..."

if command -v node >/dev/null 2>&1; then
  node start.js
else
  echo "⚠️  NodeJS not found. Running fallback bash startup..."
  
  # Run backend in background
  cd backend && cargo run &
  
  # Run frontend in background
  cd ../frontend && npm run dev &
  
  # Wait for all background tasks
  wait
fi
