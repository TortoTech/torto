import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
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

export function planAssets(source, target) {
  return source.filter(asset => {
    const matches = target.filter(file => file.name === asset.name);
    if (matches.length > 1 || (matches.length === 1 && matches[0].size !== asset.size)) {
      throw new Error(`Conflicting Gitee attachment: ${asset.name}; resolve it manually before retrying`);
    }
    return matches.length === 0;
  });
}

async function request(url, { token, method = 'GET', json, form, optional = false, binary = false } = {}) {
  const headers = { 'User-Agent': 'Torto-release-mirror', Accept: binary ? 'application/octet-stream' : 'application/json' };
  if (token) headers.Authorization = `token ${token}`;
  if (json) headers['Content-Type'] = 'application/json';
  let response;
  try {
    response = await fetch(url, {
      method, headers, body: form ?? (json ? JSON.stringify(json) : undefined),
      signal: AbortSignal.timeout(binary || form ? 600_000 : 60_000),
      // Authenticated API calls must never redirect credentials to another host.
      redirect: token ? 'error' : 'follow',
    });
    if (optional && response.status === 404) return null;
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return binary ? Buffer.from(await response.arrayBuffer()) : await response.json();
  } catch (error) {
    // Do not print response bodies, headers or nested transport errors containing credentials.
    const status = response ? `HTTP ${response.status}` : 'network/timeout failure';
    throw new Error(`${method} ${new URL(url).pathname}: ${status}`);
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

function tagCommit(tag) {
  return execFileSync('git', ['rev-parse', '--verify', `refs/tags/${tag}^{commit}`], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
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
  try {
    execFileSync('git', ['-c', 'credential.helper=', 'push', 'https://gitee.com/TortoTech/torto.git', `refs/tags/${tag}:refs/tags/${tag}`], {
      env: { ...process.env, GIT_ASKPASS: askpass, GIT_TERMINAL_PROMPT: '0', TORTO_GITEE_LOGIN: user.login, GITEE_TOKEN: token },
      stdio: ['ignore', 'pipe', 'pipe'], timeout: 600_000,
    });
  } catch {
    throw new Error('Could not push the release tag to Gitee (check token repository write permissions)');
  }
}

export async function syncRelease(release, api, download) {
  const tag = encodeURIComponent(release.tag_name);
  let target = await api('GET', `/releases/tags/${tag}`, null, true);
  const metadata = {
    tag_name: release.tag_name, name: release.name || release.tag_name,
    body: release.body || release.tag_name, prerelease: false,
    target_commitish: release.tag_name,
  };
  if (!target) target = await api('POST', '/releases', metadata);
  const files = await api('LIST', `/releases/${target.id}/attach_files`);
  const missing = planAssets(release.assets, files);
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
  try {
    await mirrorTag(release.tag_name, token, directory);
    const api = async (method, suffix, data, optional = false) => {
      if (method === 'LIST') return attachments(Number(suffix.split('/')[2]), token);
      if (method === 'UPLOAD') {
        const form = new FormData();
        form.append('file', new Blob([data.bytes]), data.asset.name);
        return request(`${gitee}${suffix}`, { token, method: 'POST', form });
      }
      return request(`${gitee}${suffix}`, { token, method, json: data, optional });
    };
    for (let attempt = 1; ; attempt++) {
      try {
        await syncRelease(release, api, asset => request(asset.browser_download_url, { binary: true }));
        console.log(`Gitee mirror verified: ${release.tag_name}`);
        break;
      } catch (error) {
        if (attempt === 3) throw error;
        console.log(`Sync attempt ${attempt} failed; retrying from remote attachment state`);
        await new Promise(resolve => setTimeout(resolve, 10_000));
      }
    }
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
