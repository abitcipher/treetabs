// Build the binaries and the extension the tests use.
import { execFileSync } from 'node:child_process';
import { ROOT } from './helpers.js';

export default function () {
  const run = (...args) => execFileSync('cargo', args, { cwd: ROOT, stdio: 'inherit' });
  run('build', '-q', '-p', 'treetabs-cli', '-p', 'treetabs-sync');
  run('xtask', 'build-ext');
}
