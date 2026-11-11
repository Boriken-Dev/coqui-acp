'use strict';
// Loaded into Pi (`node --require`, through NODE_OPTIONS) by coqui-acp-pi. Pi, and the packages it starts at startup (npm,
// git), start programs by bare name; on Windows the search for a name begins in the working directory (`which`, which
// cross-spawn uses, and `where.exe`), so a file the project holds under the name of a program would run. This file keeps
// the project out of every such search:
//
//  - the PATH of this process loses every entry that is relative, on the network, or inside COQUI_ACP_PI_KEEP_OUT (a JSON
//    array of the directories of the session);
//  - on Windows a program started by a bare name is looked for here, on that PATH, and not found means it fails to start;
//  - `where` is started in this file's own directory, which holds nothing of the project's;
//  - on Windows a program, or the script an interpreter or `cmd.exe /c` is given, that is an executable file directly in the
//    working directory is not started: that is the answer of a search that began there.
//
// Only the functions that start a program without a shell are wrapped; one that goes through a shell is searched by that
// shell, which NoDefaultCurrentDirectoryInExePath keeps out of the directory.

const childProcess = require('child_process');
const fs = require('fs');
const path = require('path');

const windows = process.platform === 'win32';
const fold = (text) => (windows ? text.toLowerCase() : text);
const EXECUTABLE = /\.(com|exe|bat|cmd)$/i;
const here = __dirname;

function parseKeepOut() {
  try {
    const list = JSON.parse(process.env.COQUI_ACP_PI_KEEP_OUT || '[]');
    return Array.isArray(list)
      ? list.filter((item) => typeof item === 'string' && item).map((item) => fold(path.resolve(item)))
      : [];
  } catch {
    return [];
  }
}

const keepOut = parseKeepOut();

function inside(text) {
  const resolved = fold(path.resolve(text));
  return keepOut.some((root) => {
    const prefix = root.endsWith(path.sep) ? root : root + path.sep;
    return resolved === root || resolved.startsWith(prefix);
  });
}

function written(entry) {
  return windows ? /^[a-zA-Z]:[\\/]/.test(entry) : path.isAbsolute(entry);
}

function allowed(entry) {
  const text = entry.replace(/^"(.*)"$/, '$1');
  return text && written(text) && !(windows && /^[\\/]{2}/.test(text)) && !inside(text);
}

function clean(pathValue) {
  return String(pathValue || '')
    .split(path.delimiter)
    .map((entry) => entry.replace(/^"(.*)"$/, '$1'))
    .filter(allowed);
}

function pathOf(env) {
  if (!env) return process.env.PATH;
  const key = Object.keys(env).find((name) => name.toUpperCase() === 'PATH');
  return key === undefined ? undefined : env[key];
}

try {
  process.env.PATH = clean(process.env.PATH).join(path.delimiter);
} catch {
  // A PATH that cannot be set stays as it is; the wrapped functions below still look names up themselves.
}

function isFile(candidate) {
  try {
    return fs.statSync(candidate).isFile();
  } catch {
    return false;
  }
}

function lookup(name, pathValue) {
  const extensions = (process.env.PATHEXT || '.COM;.EXE;.BAT;.CMD').split(';').filter(Boolean);
  const names = path.extname(name) ? [name, ...extensions.map((ext) => name + ext)] : extensions.map((ext) => name + ext);
  for (const dir of clean(pathValue)) {
    for (const candidate of names) {
      const full = path.join(dir, candidate);
      if (isFile(full)) return full;
    }
  }
  return undefined;
}

function bare(file) {
  return typeof file === 'string' && file !== '' && !/[\\/]/.test(file) && !/^[a-zA-Z]:/.test(file);
}

function refuse(message) {
  try {
    process.stderr.write('coqui-acp-pi: ' + message + '\n');
  } catch {
    // Nothing to tell it to.
  }
  return path.join(here, 'no-such-program', 'refused');
}

// The first token of what `cmd.exe /c` is given.
function payload(args) {
  const at = args.findIndex((arg) => typeof arg === 'string' && /^\/[ck]$/i.test(arg));
  const text = at >= 0 ? args[at + 1] : undefined;
  if (typeof text !== 'string') return undefined;
  const quoted = /^\s*"([^"]*)"/.exec(text);
  return quoted ? quoted[1] : text.trim().split(/\s+/)[0];
}

// Whether `candidate` is an executable file written in full that lies directly in one of the working directories: a search
// that began there found it. A name written without a directory is an argument, not a file found.
function direct(candidate, cwds) {
  if (typeof candidate !== 'string' || !EXECUTABLE.test(candidate) || !written(candidate)) return false;
  return cwds.some((cwd) => {
    const full = path.resolve(cwd, candidate);
    return fold(path.dirname(full)) === fold(path.resolve(cwd));
  });
}

function adjusted(file, args, options) {
  if (typeof file !== 'string' || (options && options.shell)) return { file, options };
  let changed = options;
  let target = file;
  const name = path.basename(file).replace(/\.(exe|com|cmd|bat)$/i, '').toLowerCase();
  if (name === 'where') {
    changed = Object.assign({}, options, { cwd: here });
  }
  if (!windows) return { file: target, options: changed };
  if (bare(target)) {
    const found = lookup(target, pathOf(options && options.env));
    target = found === undefined ? refuse('no program ' + JSON.stringify(file) + ' on a PATH outside the project') : found;
  }
  const cwds = [];
  try {
    cwds.push(process.cwd());
  } catch {
    // No working directory to compare with.
  }
  if (options && typeof options.cwd === 'string') cwds.push(options.cwd);
  const list = Array.isArray(args) ? args : [];
  const candidates = [target, list[0]];
  if (/^cmd(\.exe)?$/i.test(path.basename(target))) candidates.push(payload(list));
  if (candidates.some((candidate) => direct(candidate, cwds))) {
    target = refuse('not starting a program that is a file of the project');
  }
  return { file: target, options: changed };
}

// Splits (file, args?, options?, callback?) and calls `original` with the file and options adjusted.
function wrap(name) {
  const original = childProcess[name];
  if (typeof original !== 'function') return;
  childProcess[name] = function (file, ...rest) {
    let args = [];
    let at = 0;
    if (Array.isArray(rest[0])) {
      args = rest[0];
      at = 1;
    }
    let options;
    if (rest[at] && typeof rest[at] === 'object') {
      options = rest[at];
      at += 1;
    }
    const callback = typeof rest[at] === 'function' ? rest[at] : undefined;
    let next;
    try {
      next = adjusted(file, args, options);
    } catch {
      next = { file: refuse('a program could not be checked'), options };
    }
    const call = [next.file, args];
    if (next.options !== undefined || callback) call.push(next.options);
    if (callback) call.push(callback);
    return original.apply(this, call);
  };
}

for (const name of ['spawn', 'spawnSync', 'execFile', 'execFileSync']) wrap(name);
