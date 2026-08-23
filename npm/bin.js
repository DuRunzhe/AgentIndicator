#!/usr/bin/env node
'use strict';
const { spawnSync } = require('node:child_process');
const { join } = require('node:path');
const exe = process.platform === 'win32' ? 'agent-status-indicator.exe' : 'agent-status-indicator';
const binary = join(__dirname, '..', 'bin', `${process.platform}-${process.arch}`, exe);
const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit', detached: false });
if (result.error) {
  console.error(`No binary for ${process.platform}-${process.arch}. Install with Homebrew or download a release asset.`);
  process.exit(1);
}
process.exit(result.status ?? 1);

