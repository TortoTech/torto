import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash, createPublicKey, verify } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const github = 'https://api.github.com/repos/TortoTech/torto';
const gitee = 'https://gitee.com/api/v5/repos/TortoTech/torto';

export function requiredAssets(tag) {
  if (!/^v?\d+\.\d+\.\d+$/.test(tag)) throw new Error('Expected a stable version tag');
  const version = tag.replace(/^v/, '');
  return [
    `Torto-${version}-x86_64.msi`,
    ...['arm64', 'x86_64'].flatMap(arch => [
      `Torto-${version}-macos-${arch}.dmg`,
      `Torto-${version}-macos-${arch}.dmg.sha256`,
    ]),
  ];
}

export function validateRelease(release) {
  if (release.draft || release.prerelease) throw new Error('Only published stable releases can be mirrored');
  const required = requiredAssets(release.tag_name);
  const names = new Set(release.assets.map(asset => asset.name));
  // Historical releases predate automatic macOS updates. Once any update ZIP
  // appears, wait for both architectures and their signatures/checksums.
  if ([...names].some(name => /-macos-(?:arm64|x86_64)\.zip(?:\.|$)/.test(name))) {
    const version = release.tag_name.replace(/^v/, '');
    required.push(...['arm64', 'x86_64'].flatMap(arch => {
      const zip = `Torto-${version}-macos-${arch}.zip`;
      return [zip, `${zip}.sha256`, `${zip}.sig.json`];
    }));
  }
  // Keep historical releases mirrorable; new portable releases must include
  // their checksum before the completion manifest can be published.
  if ([...names].some(name => /-windows-x86_64-portable\.zip(?:\.|$)/.test(name))) {
    const zip = `Torto-${release.tag_name.replace(/^v/, '')}-windows-x86_64-portable.zip`;
    required.push(zip, `${zip}.sha256`);
  }
  // Older releases have no Android build. Once one APK/checksum appears,
  // require the complete split set before advertising either ABI on Gitee.
  if ([...names].some(name => /-android-(?:arm64-v8a|x86_64)\.apk(?:\.|$)/.test(name))) {
    required.push(...['arm64-v8a', 'x86_64'].flatMap(abi => {
      const apk = `Torto-${release.tag_name.replace(/^v/, '')}-android-${abi}.apk`;
      return [apk, `${apk}.sha256`];
    }));
  }
  const missing = required.filter(name => !names.has(name));
  if (missing.length) return missing;
  for (const asset of release.assets) {
    if (path.basename(asset.name) !== asset.name || /[\\"\r\n]/.test(asset.name)) {
      throw new Error('Unsafe attachment name');
    }
    if (!Number.isSafeInteger(asset.size) || asset.size <= 0 || !/^sha256:[a-f0-9]{64}$/.test(asset.digest ?? '')) {
      throw new Error(`Missing size or SHA-256 for ${asset.name}`);
    }
    const url = new URL(asset.browser_download_url);
    if (url.origin !== 'https://github.com' || !url.pathname.startsWith(`/TortoTech/torto/releases/download/${release.tag_name}/`)) {
      throw new Error('Unexpected GitHub attachment URL');
    }
  }
  return [];
}

export function verifyAsset(asset, bytes) {
  const digest = `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
  if (bytes.length !== asset.size || digest !== asset.digest) throw new Error(`Attachment verification failed: ${asset.name}`);
}

// Actions downloads do not access Release asset URLs. Always compare extracted
// files against Release digests: a rebuilt version can have several artifacts.
export function artifactName(asset) {
  const mac = /^(Torto-\d+\.\d+\.\d+-macos-(?:arm64|x86_64))\.(?:dmg(?:\.sha256)?|zip(?:\.sha256|\.sig\.json)?)$/.exec(asset.name);
  if (mac) return mac[1];
  const android = /^(Torto-\d+\.\d+\.\d+-android-(?:arm64-v8a|x86_64))\.apk(?:\.sha256)?$/.exec(asset.name);
  if (android) return android[1];
  const windows = /^(Torto-\d+\.\d+\.\d+)(?:-x86_64\.msi|-windows-x86_64-portable\.zip(?:\.sha256)?)$/.exec(asset.name);
  return windows ? `${windows[1]}-windows-x86_64` : null;
}

export function createArtifactSource(directory, token, {
  list = name => request(`${github}/actions/artifacts?name=${encodeURIComponent(name)}&per_page=100`, { token }),
  extract = (artifact, destination) => execFileSync('gh', ['run', 'download', String(artifact.workflow_run.id),
    '--repo', 'TortoTech/torto', '--name', artifact.name, '--dir', destination], {
    env: { ...process.env, GH_TOKEN: token }, stdio: ['ignore', 'pipe', 'pipe'], timeout: 600_000,
  }),
  log = console.log,
} = {}) {
  const groups = new Map();
  return async asset => {
    const name = artifactName(asset);
    if (!name || !token) return null;
    if (!groups.has(name)) {
      let candidates = [];
      try {
        const response = await list(name);
        candidates = response.artifacts.filter(item => item.name === name && !item.expired &&
          Number.isSafeInteger(item.id) && Number.isSafeInteger(item.workflow_run?.id))
          .sort((a, b) => b.id - a.id).slice(0, 3);
      } catch { log(`Actions artifacts unavailable for ${name}; Release fallback may be needed`); }
      groups.set(name, candidates.map(artifact => ({ artifact, extracted: false, unavailable: false })));
    }
    for (const candidate of groups.get(name)) {
      if (candidate.unavailable) continue;
      const destination = path.join(directory, String(candidate.artifact.id));
      if (!candidate.extracted) {
        try { await extract(candidate.artifact, destination); candidate.extracted = true; }
        catch {
          candidate.unavailable = true;
          log(`Actions artifact ${candidate.artifact.id} unavailable; checking other sources`);
          continue;
        }
      }
      try {
        const bytes = fs.readFileSync(path.join(destination, asset.name));
        verifyAsset(asset, bytes);
        return bytes;
      } catch { /* Missing or stale rebuild: never upload unverified artifact bytes. */ }
    }
    return null;
  };
}

export function createAssetDownloader(directory, {
  artifactDirectory, artifactSource = async () => null,
  download = asset => request(asset.browser_download_url, { binary: true }), log = console.log,
} = {}) {
  fs.mkdirSync(directory, { recursive: true });
  const records = new Map();
  const get = async asset => {
    // Gitee verification must read the remote file, never a cached source copy.
    if (new URL(asset.browser_download_url).hostname !== 'github.com') return download(asset);
    const key = createHash('sha256').update(`${asset.browser_download_url}\n${asset.digest}`).digest('hex');
    const file = path.join(directory, key);
    if (!records.has(key)) records.set(key, { name: asset.name, requests: 0, verified: 0, reused: 0, source: '' });
    const record = records.get(key);
    if (fs.existsSync(file)) {
      const bytes = fs.readFileSync(file);
      verifyAsset(asset, bytes);
      record.reused++;
      log(`Reusing verified source: ${asset.name}`);
      return bytes;
    }
    let bytes;
    if (artifactDirectory) {
      try {
        bytes = fs.readFileSync(path.join(artifactDirectory, asset.name));
        verifyAsset(asset, bytes);
        record.source = 'local build';
      } catch { bytes = null; }
    }
    if (!bytes) {
      bytes = await artifactSource(asset);
      if (bytes) { verifyAsset(asset, bytes); record.source = 'Actions artifact'; }
    }
    if (!bytes) {
      record.requests++;
      record.source = 'GitHub Release';
      log(`Automatic Release download request #${record.requests}: ${asset.name}`);
      bytes = await download(asset);
      verifyAsset(asset, bytes);
      record.verified++;
    }
    fs.writeFileSync(file, bytes);
    log(`Verified source ${asset.name}: ${record.source}`);
    return bytes;
  };
  get.audit = () => [...records.values()].map(record => ({ ...record }));
  return get;
}

function reportDownloads(tag, download) {
  const rows = download.audit();
  const report = [`### Gitee mirror download accounting: ${tag}`, '',
    '| File | Source | Release requests | Verified Release downloads | Cache reuse |',
    '| --- | --- | ---: | ---: | ---: |',
    ...rows.map(row => `| ${row.name.replaceAll('|', '\\|')} | ${row.source} | ${row.requests} | ${row.verified} | ${row.reused} |`), '',
    'Requests include failed attempts; GitHub exposes no exact per-request download-count attribution. Gitee verification downloads are separate.', ''].join('\n');
  console.log(report);
  if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, report);
}

export function planAssets(source, target) {
  return source.filter(asset => {
    const matches = target.filter(file => file.name === asset.name);
    if (matches.length > 1 || (matches.length === 1 && matches[0].size !== asset.size)) {
      throw new Error(`Conflicting Gitee attachment: ${asset.name}; resolve it manually before retrying`);
    }
    return matches.length === 0;
  });
}

export function updateManifest(release, portable = false) {
  const name = `Torto-${release.tag_name.replace(/^v/, '')}${portable ? '-windows-x86_64-portable.zip' : '-x86_64.msi'}`;
  return installerManifest(release, name, portable ? 'torto-windows-portable-update.json' : 'torto-update.json');
}

export function androidUpdateManifest(release, abi) {
  if (!['arm64-v8a', 'x86_64'].includes(abi)) throw new Error('Unsupported Android architecture');
  const name = `Torto-${release.tag_name.replace(/^v/, '')}-android-${abi}.apk`;
  return installerManifest(release, name, abi === 'arm64-v8a' ? 'torto-android-update.json' : 'torto-android-x86_64-update.json');
}

function installerManifest(release, name, marker) {
  const installer = release.assets.find(asset => asset.name === name);
  if (!installer || !Number.isSafeInteger(installer.size) || installer.size <= 0 ||
      !/^sha256:[a-f0-9]{64}$/.test(installer.digest ?? '')) throw new Error('Missing or unverified installer for update manifest');
  const bytes = Buffer.from(JSON.stringify({
    version: release.tag_name.replace(/^v/, ''), tag: release.tag_name,
    asset: { name, size: installer.size, sha256: installer.digest.replace(/^sha256:/, '') },
  }) + '\n');
  return { bytes, asset: { name: marker, size: bytes.length,
    digest: `sha256:${createHash('sha256').update(bytes).digest('hex')}` } };
}

export function macosUpdateManifest(release, arch, signature) {
  if (!['arm64', 'x86_64'].includes(arch)) throw new Error('Unsupported macOS architecture');
  const version = release.tag_name.replace(/^v/, '');
  const name = `Torto-${version}-macos-${arch}.zip`;
  const installer = release.assets.find(asset => asset.name === name);
  const sha256 = installer?.digest?.replace(/^sha256:/, '');
  if (!installer || signature.version !== version || signature.name !== name ||
      signature.size !== installer.size || signature.sha256 !== sha256 ||
      !/^[a-f0-9]{64}$/.test(sha256) || installer.size > 256 * 1024 * 1024) {
    throw new Error('Signed macOS metadata does not match the release');
  }
  const hex = fs.readFileSync(new URL('../assets/macos/update-public-key.hex', import.meta.url), 'utf8').trim();
  if (!/^[a-f0-9]{64}$/.test(hex)) throw new Error('Invalid macOS update public key');
  const key = createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), Buffer.from(hex, 'hex')]), format: 'der', type: 'spki' });
  const message = `TORTO-MACOS-UPDATE-V1\n${version}\n${name}\n${installer.size}\n${sha256}\n`;
  if (typeof signature.signature !== 'string' || !verify(null, Buffer.from(message), key, Buffer.from(signature.signature, 'base64'))) {
    throw new Error('Invalid macOS publisher signature');
  }
  const bytes = Buffer.from(JSON.stringify({version, tag: release.tag_name,
    asset: {name, size: installer.size, sha256}, macos_signature: signature}) + '\n');
  return {bytes, asset: {name: `torto-macos-${arch}-update.json`, size: bytes.length,
    digest: `sha256:${createHash('sha256').update(bytes).digest('hex')}`}};
}

const transportCodes = new Set([
  'UND_ERR_CONNECT_TIMEOUT', 'UND_ERR_HEADERS_TIMEOUT', 'UND_ERR_BODY_TIMEOUT',
  'UND_ERR_SOCKET', 'UND_ERR_ABORTED', 'UND_ERR_REQ_CONTENT_LENGTH_MISMATCH',
  'ECONNRESET', 'ECONNREFUSED', 'ETIMEDOUT', 'EPIPE', 'ENOTFOUND', 'EAI_AGAIN',
  'ENETUNREACH', 'EHOSTUNREACH', 'CERT_HAS_EXPIRED', 'UNABLE_TO_VERIFY_LEAF_SIGNATURE',
]);
const transportNames = new Set(['Error', 'TypeError', 'TimeoutError', 'AbortError', 'AggregateError', 'SyntaxError']);

function transportDiagnostics(error, seen = new Set()) {
  if (!error || typeof error !== 'object' || seen.has(error) || seen.size >= 8) return [];
  seen.add(error);
  const details = [];
  if (transportNames.has(error.name)) details.push(error.name);
  if (transportCodes.has(error.code)) details.push(error.code);
  details.push(...transportDiagnostics(error.cause, seen));
  if (Array.isArray(error.errors)) {
    for (const nested of error.errors.slice(0, 8)) details.push(...transportDiagnostics(nested, seen));
  }
  return [...new Set(details)];
}

export async function request(url, { token, method = 'GET', json, form, optional = false, binary = false } = {}) {
  const headers = { 'User-Agent': 'Torto-release-mirror', Accept: binary ? 'application/octet-stream' : 'application/json' };
  if (token) headers.Authorization = `${new URL(url).hostname === 'gitee.com' ? 'Bearer' : 'token'} ${token}`;
  if (json) headers['Content-Type'] = 'application/json';
  const started = performance.now();
  const timeoutMs = binary || form ? 3_600_000 : 60_000;
  let phase = 'waiting-for-response-headers';
  let response;
  try {
    response = await fetch(url, {
      method, headers, body: form ?? (json ? JSON.stringify(json) : undefined),
      signal: AbortSignal.timeout(timeoutMs),
      // Authenticated API calls must never redirect credentials to another host.
      redirect: token ? 'error' : 'follow',
    });
    if (optional && response.status === 404) return null;
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    phase = 'reading-response-body';
    return binary ? Buffer.from(await response.arrayBuffer()) : await response.json();
  } catch (error) {
    // Only allowlisted names/codes: never serialize transport objects or messages,
    // which can contain authenticated URLs, headers or response bodies.
    const status = response ? `HTTP ${response.status}` : 'network/timeout failure';
    const elapsedMs = Math.round(performance.now() - started);
    const details = transportDiagnostics(error).join(', ') || 'unknown';
    throw new Error(`${method} ${new URL(url).pathname}: ${status}; phase=${phase}; elapsed_ms=${elapsedMs}; timeout_ms=${timeoutMs}; transport=${details}`);
  }
}

async function attachments(id, token) {
  const files = [];
  for (let page = 1; ; page++) {
    const batch = await request(`${gitee}/releases/${id}/attach_files?per_page=100&page=${page}`, { token });
    files.push(...batch);
    if (batch.length < 100) return files;
  }
}

export async function uploadAttachment(url, token, { asset, bytes }, { log = console.log, timeoutSeconds = 3600 } = {}) {
  verifyAsset(asset, bytes);
  if (/[\r\n\0]/.test(token)) throw new Error('Invalid Gitee token');
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'torto-upload-'));
  const file = path.join(directory, 'attachment');
  const responseFile = path.join(directory, 'response.json');
  const quote = value => `"${value.replaceAll('\\', '\\\\').replaceAll('"', '\\"')}"`;
  const started = performance.now();
  try {
    fs.writeFileSync(file, bytes, { mode: 0o600 });
    log(`Uploading ${asset.name} (${bytes.length} bytes) with curl`);
    const stats = ['http_code', 'size_upload', 'speed_upload', 'time_connect', 'time_starttransfer', 'time_total'];
    const args = ['--disable', '--config', '-', '--silent', '--show-error',
      '--connect-timeout', '30', '--max-time', String(timeoutSeconds),
      '--user-agent', 'Torto-release-mirror', '--header', 'Accept: application/json',
      '--form', `file=@${quote(file)};filename=${quote(asset.name)}`,
      '--output', responseFile, '--write-out', stats.map(key => `%{${key}}`).join(' '), url];
    const result = await new Promise((resolve, reject) => {
      const child = spawn('curl', args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
      let output = '';
      child.stdout.on('data', chunk => { if (output.length < 4096) output += chunk.toString(); });
      // Consume but never print curl errors: they can include authenticated URLs.
      child.stderr.resume();
      child.stdin.on('error', () => {});
      child.on('error', () => reject(new Error('Could not start curl for Gitee upload')));
      child.on('close', (code, signal) => resolve({ code, signal, output }));
      // Keep credentials out of argv and logs. Never follow upload redirects.
      child.stdin.end(`form-string = ${quote(`access_token=${token}`)}\n`);
    });
    const values = result.output.trim().split(/\s+/);
    const metrics = values.length === stats.length && values.every(value => /^\d+(\.\d+)?$/.test(value))
      ? stats.map((key, index) => `${key}=${values[index]}`).join('; ') : 'metrics=unavailable';
    log(`Upload result ${asset.name}: curl_exit=${result.code}; elapsed_ms=${Math.round(performance.now() - started)}; ${metrics}`);
    const httpCode = Number(values[0]);
    if (result.code !== 0 || result.signal || !(httpCode >= 200 && httpCode < 300)) {
      throw new Error(`Gitee upload failed for ${asset.name}: curl_exit=${result.code}; ${metrics}`);
    }
    try { return JSON.parse(fs.readFileSync(responseFile, 'utf8')); }
    catch { throw new Error(`Invalid Gitee upload response for ${asset.name}`); }
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
}

function tagCommit(tag) {
  return execFileSync('git', ['rev-parse', '--verify', `refs/tags/${tag}^{commit}`], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
}

export function gitPushDiagnostics(error) {
  // Never print arbitrary stderr: remote servers can echo credentials or URLs.
  // Preserve only numeric exit information and recognized transport messages.
  const stderr = String(error?.stderr || '');
  const details = [];
  if (Number.isInteger(error?.status)) details.push(`exit=${error.status}`);
  if (/^SIG[A-Z0-9]+$/.test(error?.signal || '')) details.push(`signal=${error.signal}`);
  if (['ETIMEDOUT', 'ENOENT', 'ENOBUFS'].includes(error?.code)) details.push(`code=${error.code}`);
  const patterns = [
    ['authentication-failed', /Authentication failed|could not read Username|could not read Password/i],
    ['permission-denied', /permission denied|access denied|not allowed to push|403 Forbidden/i],
    ['connection-timeout', /timed out|timeout was reached/i],
    ['connection-reset', /connection reset|recv failure/i],
    ['connection-refused', /failed to connect|connection refused/i],
    ['dns-failure', /could not resolve host/i],
    ['tls-failure', /SSL certificate|SSL connect error|TLS connection|SSL_ERROR/i],
    ['http2-failure', /HTTP\/2 stream|HTTP2 framing/i],
    ['rpc-failed', /RPC failed/i],
    ['remote-disconnected', /remote end hung up|unexpected disconnect/i],
    ['remote-rejected', /remote rejected|pre-receive hook declined/i],
    ['ref-conflict', /already exists|would clobber|non-fast-forward/i],
    ['repository-not-found', /repository .*not found/i],
  ];
  for (const [label, pattern] of patterns) if (pattern.test(stderr)) details.push(label);
  for (const match of stderr.matchAll(/(?:HTTP(?: error)?[: ]+|returned error: )(\d{3})\b/gi)) details.push(`http=${match[1]}`);
  for (const match of stderr.matchAll(/\bcurl[ :]+(\d{1,3})\b/gi)) details.push(`curl=${match[1]}`);
  details.push(`stderr_bytes=${Buffer.byteLength(stderr)}`);
  return [...new Set(details)].join('; ');
}

async function mirrorTag(tag, token, directory) {
  const commit = tagCommit(tag);
  let remote;
  for (let page = 1; ; page++) {
    const batch = await request(`${gitee}/tags?per_page=100&page=${page}`, { token });
    remote = batch.find(entry => entry.name === tag);
    if (remote || batch.length < 100) break;
  }
  if (remote) {
    if (remote.commit.sha !== commit) throw new Error(`Gitee tag ${tag} points to a different commit; refusing to overwrite it`);
    return;
  }
  const user = await request('https://gitee.com/api/v5/user', { token });
  const askpass = path.join(directory, 'askpass.sh');
  fs.writeFileSync(askpass, '#!/bin/sh\ncase "$1" in *Username*) printf "%s\\n" "$TORTO_GITEE_LOGIN" ;; *) printf "%s\\n" "$GITEE_TOKEN" ;; esac\n', { mode: 0o700 });
  const started = performance.now();
  try {
    console.log(`Pushing Gitee tag ${tag} at ${commit}; timeout_ms=600000`);
    execFileSync('git', ['-c', 'credential.helper=', 'push', 'https://gitee.com/TortoTech/torto.git', `refs/tags/${tag}:refs/tags/${tag}`], {
      env: { ...process.env, GIT_ASKPASS: askpass, GIT_TERMINAL_PROMPT: '0', TORTO_GITEE_LOGIN: user.login, GITEE_TOKEN: token },
      stdio: ['ignore', 'pipe', 'pipe'], timeout: 600_000,
    });
    console.log(`Gitee tag push completed: ${tag}; elapsed_ms=${Math.round(performance.now() - started)}`);
  } catch (error) {
    throw new Error(`Could not push the release tag to Gitee: elapsed_ms=${Math.round(performance.now() - started)}; ${gitPushDiagnostics(error)}`);
  }
}

export async function syncRelease(release, api, download) {
  const incomplete = validateRelease(release);
  if (incomplete.length) throw new Error(`Release is not complete: ${incomplete.join(', ')}`);
  const manifests = [updateManifest(release)];
  if (release.assets.some(asset => asset.name.endsWith('-windows-x86_64-portable.zip'))) {
    manifests.push(updateManifest(release, true));
  }
  for (const abi of ['arm64-v8a', 'x86_64']) {
    if (release.assets.some(asset => asset.name.endsWith(`-android-${abi}.apk`))) {
      manifests.push(androidUpdateManifest(release, abi));
    }
  }
  for (const arch of ['arm64', 'x86_64']) {
    const name = `Torto-${release.tag_name.replace(/^v/, '')}-macos-${arch}.zip`;
    if (!release.assets.some(asset => asset.name === name)) continue;
    const asset = release.assets.find(asset => asset.name === `${name}.sig.json`);
    if (!asset || asset.size > 8192) throw new Error('Missing or oversized macOS update signature');
    const bytes = await download(asset);
    verifyAsset(asset, bytes);
    manifests.push(macosUpdateManifest(release, arch, JSON.parse(bytes)));
  }
  const tag = encodeURIComponent(release.tag_name);
  let target = await api('GET', `/releases/tags/${tag}`, null, true);
  const metadata = {
    tag_name: release.tag_name, name: release.name || release.tag_name,
    body: release.body || release.tag_name, prerelease: false,
    target_commitish: release.tag_name,
  };
  if (!target) target = await api('POST', '/releases', metadata);
  const files = await api('LIST', `/releases/${target.id}/attach_files`);
  // Test the same authenticated upload path with small real checksum files first.
  const missing = planAssets(release.assets, files).sort((a, b) => a.size - b.size);
  for (const asset of missing) {
    const bytes = await download(asset);
    verifyAsset(asset, bytes);
    await api('UPLOAD', `/releases/${target.id}/attach_files`, { asset, bytes });
    console.log(`Uploaded ${asset.name}`);
  }
  // Verify uploaded and pre-existing attachments, including same-size corruption.
  const final = await api('LIST', `/releases/${target.id}/attach_files`);
  if (planAssets(release.assets, final).length) throw new Error('Gitee is still missing attachments');
  for (const asset of release.assets) {
    const url = `https://gitee.com/TortoTech/torto/releases/download/${tag}/${encodeURIComponent(asset.name)}`;
    verifyAsset(asset, await download({ ...asset, browser_download_url: url }));
  }
  // Completion marker for clients: publish only after all installers are verified.
  for (const manifest of manifests) {
    if (planAssets([manifest.asset], final).length) {
      await api('UPLOAD', `/releases/${target.id}/attach_files`, manifest);
    }
    verifyAsset(manifest.asset, await download({ ...manifest.asset,
      browser_download_url: `https://gitee.com/TortoTech/torto/releases/download/${tag}/${manifest.asset.name}` }));
  }
  // Update notes after every attachment has been verified; reruns also propagate edited notes.
  await api('PATCH', `/releases/${target.id}`, metadata);
}

async function main() {
  const args = process.argv.slice(2);
  const value = key => args.includes(key) ? args[args.indexOf(key) + 1] : undefined;
  const githubToken = process.env.GH_TOKEN || process.env.GITHUB_TOKEN;
  const token = process.env.GITEE_TOKEN;
  const commit = value('--commit');
  let release;
  if (commit) {
    if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('Invalid workflow commit');
    // Only tags on the completed build commit are eligible; ordinary main builds do nothing.
    const tags = execFileSync('git', ['tag', '--points-at', commit], { encoding: 'utf8' }).trim().split('\n').filter(tag => /^v?\d+\.\d+\.\d+$/.test(tag));
    for (const tag of tags) {
      release = await request(`${github}/releases/tags/${encodeURIComponent(tag)}`, { token: githubToken, optional: true });
      if (release && !release.draft && !release.prerelease) break;
      release = null;
    }
    if (!release) { console.log('No published stable release for this build; skipping'); return; }
  } else {
    const tag = value('--tag') || 'latest';
    release = await request(`${github}/releases/${tag === 'latest' ? 'latest' : `tags/${encodeURIComponent(tag)}`}`, { token: githubToken });
  }
  const missing = validateRelease(release);
  if (missing.length) {
    const message = `Release ${release.tag_name} is not complete yet: ${missing.join(', ')}`;
    if (args.includes('--skip-incomplete')) { console.log(`${message}; skipping until the other build finishes`); return; }
    throw new Error(message);
  }
  if (args.includes('--dry-run')) {
    console.log(`Ready to mirror ${release.tag_name}: ${release.assets.length} verified-metadata attachments`);
    return;
  }
  if (!token) throw new Error('GITEE_TOKEN is required');
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'torto-gitee-'));
  const download = createAssetDownloader(path.join(directory, 'source-cache'), {
    artifactDirectory: value('--artifact-dir'),
    artifactSource: createArtifactSource(path.join(directory, 'artifacts'), githubToken),
  });
  try {
    await mirrorTag(release.tag_name, token, directory);
    const api = async (method, suffix, data, optional = false) => {
      if (method === 'LIST') return attachments(Number(suffix.split('/')[2]), token);
      if (method === 'UPLOAD') {
        return uploadAttachment(`${gitee}${suffix}`, token, data);
      }
      return request(`${gitee}${suffix}`, { token, method, json: data, optional });
    };
    for (let attempt = 1; ; attempt++) {
      try {
        await syncRelease(release, api, download);
        console.log(`Gitee mirror verified: ${release.tag_name}`);
        break;
      } catch (error) {
        if (attempt === 3) throw error;
        console.log(`Sync attempt ${attempt} failed: ${error.message}; retrying from remote attachment state`);
        await new Promise(resolve => setTimeout(resolve, 10_000));
      }
    }
  } finally {
    reportDownloads(release.tag_name, download);
    fs.rmSync(directory, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
