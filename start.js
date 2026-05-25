const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const logo = `
\x1b[38;5;208m██████╗ ██╗   ██╗███████╗████████╗    ██████╗  ██████╗ ██████╗ ███████╗\x1b[0m
\x1b[38;5;208m██╔══██╗██║   ██║██╔════╝╚══██╔══╝   ██╔════╝ ██╔═══██╗██╔══██╗██╔════╝\x1b[0m
\x1b[38;5;208m██████╔╝██║   ██║███████╗   ██║      ██║      ██║   ██║██║  ██║█████╗  \x1b[0m
\x1b[38;5;208m██╔══██╗██║   ██║╚════██║   ██║      ██║      ██║   ██║██║  ██║██╔══╝  \x1b[0m
\x1b[38;5;208m██║  ██║╚██████╔╝███████║   ██║      ╚██████╗ ╚██████╔╝██████╔╝███████╗\x1b[0m
\x1b[38;5;208m╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝       ╚═════╝  ╚═════╝ ╚═════╝ ╚══════╝\x1b[0m
                     \x1b[36mS  T  U  D  I  O\x1b[0m

\x1b[36m🚀 Starting Rust Code Studio in RELEASE mode (Production Build)... \x1b[0m
`;
console.log(logo);

let backend;
let frontend;
let activeBuilds = [];

const frontendDir = path.join(__dirname, 'frontend');
const backendDir = path.join(__dirname, 'backend');
const nodeModulesExist = fs.existsSync(path.join(frontendDir, 'node_modules'));

// Helper to log stdout with prefixes
function logOutput(proc, name, color) {
  proc.stdout.on('data', (data) => {
    const lines = data.toString().split('\n');
    lines.forEach(line => {
      if (line.trim()) {
        console.log(`${color}[${name}]\x1b[0m ${line}`);
      }
    });
  });

  proc.stderr.on('data', (data) => {
    const lines = data.toString().split('\n');
    lines.forEach(line => {
      if (line.trim()) {
        console.error(`\x1b[31m[${name} ERR]\x1b[0m ${line}`);
      }
    });
  });
}

function runCommand(command, args, cwd, name, color) {
  return new Promise((resolve, reject) => {
    console.log(`${color}[${name}]\x1b[0m Starting: ${command} ${args.join(' ')}`);
    const proc = spawn(command, args, { cwd, shell: true });
    activeBuilds.push(proc);
    logOutput(proc, name, color);
    proc.on('close', (code) => {
      activeBuilds = activeBuilds.filter(p => p !== proc);
      if (code === 0) {
        console.log(`${color}[${name}]\x1b[0m completed successfully!`);
        resolve();
      } else {
        console.error(`\x1b[31m[${name} ERR]\x1b[0m failed with exit code ${code}`);
        reject(new Error(`${name} failed`));
      }
    });
  });
}

async function buildAndStart() {
  try {
    console.log('\x1b[36m📦 Building frontend and backend in production release mode...\x1b[0m');
    
    // Build concurrently
    await Promise.all([
      runCommand('npm', ['run', 'build'], frontendDir, 'Frontend Build', '\x1b[35m'),
      runCommand('cargo', ['build', '--release'], backendDir, 'Backend Build', '\x1b[32m')
    ]);

    console.log('\x1b[32m✅ Production release builds completed successfully!\x1b[0m');
    console.log('\x1b[36m🚀 Spawning release servers...\x1b[0m');

    // Run backend: try to spawn the compiled binary directly, otherwise fall back to cargo run --release
    const isWin = process.platform === 'win32';
    const binaryName = isWin ? 'rust_no_code_studio.exe' : 'rust_no_code_studio';
    const binaryPath = path.join(backendDir, 'target', 'release', binaryName);

    if (fs.existsSync(binaryPath)) {
      console.log(`\x1b[32m[Backend]\x1b[0m Running compiled release binary: ${binaryPath}`);
      backend = spawn(binaryPath, [], { cwd: backendDir, shell: true });
    } else {
      console.log('\x1b[32m[Backend]\x1b[0m Falling back to cargo run --release');
      backend = spawn('cargo', ['run', '--release'], { cwd: backendDir, shell: true });
    }

    // Run frontend: npm run preview
    console.log('\x1b[35m[Frontend]\x1b[0m Running vite production preview...');
    frontend = spawn('npm', ['run', 'preview'], { cwd: frontendDir, shell: true });

    logOutput(backend, 'Backend', '\x1b[32m');
    logOutput(frontend, 'Frontend', '\x1b[35m');

  } catch (error) {
    console.error('\x1b[31m❌ Release compilation failed. Aborting startup.\x1b[0m');
    process.exit(1);
  }
}

if (!nodeModulesExist) {
  console.log('\x1b[33m%s\x1b[0m', '⚠️  node_modules not found in frontend folder. Running npm install first...');
  const install = spawn('npm', ['install'], {
    cwd: frontendDir,
    shell: true,
    stdio: 'inherit'
  });

  install.on('close', (code) => {
    if (code === 0) {
      console.log('\x1b[32m%s\x1b[0m', '✅ npm install completed successfully!');
      buildAndStart();
    } else {
      console.error('\x1b[31m%s\x1b[0m', `❌ npm install failed with exit code ${code}.`);
      process.exit(code);
    }
  });
} else {
  buildAndStart();
}

function cleanup() {
  console.log('\n\x1b[33m🛑 Stopping all services and builds...\x1b[0m');
  
  // Kill any active builds
  activeBuilds.forEach(proc => {
    try {
      proc.kill();
    } catch(e) {}
  });

  // Kill running servers
  try {
    if (process.platform === 'win32') {
      if (backend && backend.pid) spawn('taskkill', ['/pid', backend.pid, '/f', '/t']);
      if (frontend && frontend.pid) spawn('taskkill', ['/pid', frontend.pid, '/f', '/t']);
    } else {
      if (backend && backend.pid) backend.kill('SIGINT');
      if (frontend && frontend.pid) frontend.kill('SIGINT');
    }
  } catch (e) {
    // Ignore errors during exit
  }
  process.exit();
}

process.on('SIGINT', cleanup);
process.on('SIGTERM', cleanup);
