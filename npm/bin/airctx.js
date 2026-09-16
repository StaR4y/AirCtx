#!/usr/bin/env node
// AirCtx npm 启动器:按平台定位二进制并透传执行。
// 二进制由 CI 从 Rust release 产物打进 @airctx/cli-<platform> 平台子包。

'use strict';
const { spawn } = require('child_process');
const os = require('os');

const MAP = {
  'darwin-arm64': '@airctx/cli-darwin-arm64',
  'darwin-x64': '@airctx/cli-darwin-x64',
  'linux-arm64': '@airctx/cli-linux-arm64',
  'linux-x64': '@airctx/cli-linux-x64',
  'win32-x64': '@airctx/cli-win32-x64',
};

const pkgName = MAP[`${os.platform()}-${os.arch()}`];
if (!pkgName) {
  console.error(`airctx: unsupported platform ${os.platform()}-${os.arch()}`);
  process.exit(1);
}

let bin;
try {
  const path = require('path');
  const suffix = os.platform() === 'win32' ? '.exe' : '';
  bin = path.join(
    __dirname,
    '..',
    'node_modules',
    pkgName,
    'bin',
    `airctx${suffix}`
  );
  require('fs').accessSync(bin, require('fs').constants.X_OK);
} catch (e) {
  console.error(`airctx: platform binary missing or not executable (${pkgName})`);
  console.error(`try: npm install -g ${pkgName}`);
  process.exit(1);
}

const child = spawn(bin, process.argv.slice(2), { stdio: 'inherit' });
child.on('error', (err) => {
  console.error('airctx: failed to start', err);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  process.exit(signal ? 128 + 1 : (code ?? 0));
});
