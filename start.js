const { spawn } = require('child_process');
const path = require('path');

const logo = `
\x1b[38;5;208m██████╗ ██╗   ██╗███████╗████████╗    ██████╗  ██████╗ ██████╗ ███████╗\x1b[0m
\x1b[38;5;208m██╔══██╗██║   ██║██╔════╝╚══██╔══╝   ██╔════╝ ██╔═══██╗██╔══██╗██╔════╝\x1b[0m
\x1b[38;5;208m██████╔╝██║   ██║███████╗   ██║      ██║      ██║   ██║██║  ██║█████╗  \x1b[0m
\x1b[38;5;208m██╔══██╗██║   ██║╚════██║   ██║      ██║      ██║   ██║██║  ██║██╔══╝  \x1b[0m
\x1b[38;5;208m██║  ██║╚██████╔╝███████║   ██║      ╚██████╗ ╚██████╔╝██████╔╝███████╗\x1b[0m
\x1b[38;5;208m╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝       ╚═════╝  ╚═════╝ ╚═════╝ ╚══════╝\x1b[0m
                     \x1b[36mS  T  U  D  I  O\x1b[0m

\x1b[36m🚀 Starting Rust Code Studio (Backend + Frontend)... \x1b[0m
`;
console.log(logo);

const backend = spawn('cargo', ['run'], {
  cwd: path.join(__dirname, 'backend'),
  shell: true
});

const frontend = spawn('npm', ['run', 'dev'], {
  cwd: path.join(__dirname, 'frontend'),
  shell: true
});

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

logOutput(backend, 'Backend', '\x1b[32m'); // Green
logOutput(frontend, 'Frontend', '\x1b[35m'); // Magenta

function cleanup() {
  console.log('\n\x1b[33m🛑 Stopping all services...\x1b[0m');
  try {
    if (process.platform === 'win32') {
      // Clean tree kill for Windows
      spawn('taskkill', ['/pid', backend.pid, '/f', '/t']);
      spawn('taskkill', ['/pid', frontend.pid, '/f', '/t']);
    } else {
      // Graceful signal kill for POSIX
      backend.kill('SIGINT');
      frontend.kill('SIGINT');
    }
  } catch (e) {
    // Ignore errors during exit
  }
  process.exit();
}

process.on('SIGINT', cleanup);
process.on('SIGTERM', cleanup);
