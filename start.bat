@echo off
echo 🚀 Starting Rust Code Studio (Windows)...

where node >nul 2>nul
if %errorlevel% equ 0 (
  node start.js
) else (
  echo ⚠️  NodeJS not found. Running fallback batch startup...
  start cmd /k "cd backend && cargo run"
  start cmd /k "cd frontend && npm run dev"
)
