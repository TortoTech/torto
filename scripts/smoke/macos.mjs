import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { spawn, spawnSync } from 'node:child_process';
import { createFixtures } from './fixtures.mjs';

if (process.platform !== 'darwin') throw new Error('This runner checks a real macOS application');
const app = path.resolve(process.argv[2]), output = path.resolve(process.argv[3]);
const fixtures = path.join(output, 'fixtures');
createFixtures(fixtures);
fs.writeFileSync(path.join(output, 'environment.txt'), `${os.type()} ${os.release()} ${os.arch()}\n${spawnSync('sw_vers', {encoding:'utf8'}).stdout}`);
const summary = [];
for (const [name, book, finder] of [['shelf', null, false], ['epub', 'startup.epub', false], ['pdf', 'startup.pdf', false], ['launch-services', null, true]]) {
  const dir = path.join(output, name);
  fs.mkdirSync(dir); // A reused result/profile must never produce a false pass.
  const args = ['--smoke-test', dir, ...(book ? [path.join(fixtures, book)] : [])];
  const stdout = fs.openSync(path.join(dir, 'stdout.log'), 'w'), stderr = fs.openSync(path.join(dir, 'stderr.log'), 'w');
  const started = Date.now();
  let timedOut = false, screenshot = false;
  let appPid;
  const child = spawn(finder ? '/usr/bin/open' : path.join(app, 'Contents/MacOS/torto'), finder ? ['-n', '-W', '-a', app, '--args', ...args] : args, {stdio:['ignore', stdout, stderr]});
  const timer = setTimeout(() => {
    timedOut = true;
    // For Launch Services the app is not a child of `open`. The app records its
    // PID in this fresh scenario directory before initializing any services.
    if (finder) {
      try { appPid = JSON.parse(fs.readFileSync(path.join(dir, 'pid.json'))).pid; process.kill(appPid, 'SIGTERM'); } catch {}
    }
    child.kill('SIGTERM');
    setTimeout(() => {
      if (child.exitCode === null) child.kill('SIGKILL');
      if (appPid) {
        const command = spawnSync('/bin/ps', ['-p', String(appPid), '-o', 'command='], {encoding:'utf8'}).stdout?.trim();
        if (command?.includes(app) && command.includes(dir)) { try { process.kill(appPid, 'SIGKILL'); } catch {} }
      }
    }, 3000);
  }, 90000);
  const capture = setInterval(() => {
    if (screenshot) return;
    try {
      if (fs.readFileSync(path.join(dir, 'stage.txt'), 'utf8') === 'content-presented') {
        screenshot = true;
        spawnSync('/usr/sbin/screencapture', ['-x', path.join(dir, 'screen.png')], {timeout:5000});
      }
    } catch {}
  }, 1000);
  let launchError;
  const exit = await new Promise(resolve => {
    child.on('error', error => { launchError = String(error); resolve({code:null, signal:null}); });
    child.on('close', (code, signal) => resolve({code, signal}));
  });
  clearTimeout(timer); clearInterval(capture); fs.closeSync(stdout); fs.closeSync(stderr);
  if (!screenshot) spawnSync('/usr/sbin/screencapture', ['-x', path.join(dir, 'screen.png')], {timeout:5000});
  let report;
  try { report = JSON.parse(fs.readFileSync(path.join(dir, 'result.json'))); } catch {}
  const crashes = [];
  for (const folder of [path.join(os.homedir(), 'Library/Logs/DiagnosticReports'), '/Library/Logs/DiagnosticReports']) {
    for (const name of fs.existsSync(folder) ? fs.readdirSync(folder) : []) {
      const file = path.join(folder, name);
      if (/^torto.*\.(ips|crash)$/i.test(name) && fs.statSync(file).mtimeMs >= started) {
        crashes.push(name); fs.copyFileSync(file, path.join(dir, name));
      }
    }
  }
  const success = !timedOut && !launchError && exit.code === 0 && report?.success === true && crashes.length === 0;
  const result = {name, success, timedOut, launchError, ...exit, report, crashes};
  summary.push(result); fs.writeFileSync(path.join(output, 'summary.json'), JSON.stringify(summary, null, 2));
  console.log(`${success ? 'PASS' : 'FAIL'} ${name}`);
}
if (summary.some(result => !result.success)) process.exitCode = 1;
