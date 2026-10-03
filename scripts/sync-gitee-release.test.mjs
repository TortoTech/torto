import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createArtifactSource, createAssetDownloader, artifactName } from './sync-gitee-release.mjs';
import { requiredAssets, validateRelease, verifyAsset, planAssets, syncRelease, updateManifest, request, uploadAttachment } from './sync-gitee-release.mjs';

const bytes = Buffer.from('installer fixture');
const digest = `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
const release = () => ({
  tag_name: 'v0.7.0', name: 'Torto 0.7.0', body: 'Updated notes', draft: false, prerelease: false,
  assets: requiredAssets('v0.7.0').map(name => ({ name, size: bytes.length, digest,
    browser_download_url: `https://github.com/TortoTech/torto/releases/download/v0.7.0/${name}` })),
});

function temporary(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'torto-source-test-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return directory;
}

test('verified Release downloads survive upload retries while remote verification bypasses source cache', async t => {
  const asset = release().assets[0];
  const calls = [];
  const download = createAssetDownloader(temporary(t), {
    download: async item => { calls.push(item.browser_download_url); return bytes; }, log: () => {},
  });
  assert.deepEqual(await download(asset), bytes);
  assert.deepEqual(await download(asset), bytes);
  const remote = { ...asset, browser_download_url: 'https://gitee.com/remote.msi' };
  await download(remote);
  await download(remote);
  assert.deepEqual(calls, [asset.browser_download_url, remote.browser_download_url, remote.browser_download_url]);
  assert.deepEqual(download.audit(), [{ name: asset.name, requests: 1, verified: 1, reused: 1, source: 'GitHub Release' }]);
});

test('corrupt downloads are never cached and failed requests remain in accounting', async t => {
  let attempt = 0;
  const download = createAssetDownloader(temporary(t), {
    download: async () => { attempt++; return attempt === 1 ? Buffer.alloc(bytes.length) : bytes; }, log: () => {},
  });
  await assert.rejects(download(release().assets[0]), /verification failed/);
  await download(release().assets[0]);
  assert.equal(attempt, 2);
  assert.equal(download.audit()[0].requests, 2);
  assert.equal(download.audit()[0].verified, 1);
});

test('local build files avoid Release downloads and stale local files fall back to Actions', async t => {
  const directory = temporary(t);
  const local = path.join(directory, 'dist');
  fs.mkdirSync(local);
  const assets = release().assets;
  fs.writeFileSync(path.join(local, assets[0].name), bytes);
  fs.writeFileSync(path.join(local, assets[1].name), Buffer.alloc(bytes.length));
  let artifactCalls = 0;
  const download = createAssetDownloader(path.join(directory, 'cache'), {
    artifactDirectory: local,
    artifactSource: async () => { artifactCalls++; return bytes; },
    download: async () => { throw new Error('Release download must not happen'); }, log: () => {},
  });
  await download(assets[0]);
  await download(assets[1]);
  assert.equal(artifactCalls, 1);
  assert.deepEqual(download.audit().map(row => row.source), ['local build', 'Actions artifact']);
  assert.ok(download.audit().every(row => row.requests === 0));
});

test('Actions selection rejects stale rebuilds, skips expired artifacts and extracts a group only once', async t => {
  const assets = release().assets.slice(1, 3);
  const name = artifactName(assets[0]);
  assert.equal(name, 'Torto-0.7.0-macos-arm64');
  assert.equal(artifactName(release().assets[0]), 'Torto-0.7.0-windows-x86_64');
  let lists = 0;
  const extracted = [];
  const source = createArtifactSource(temporary(t), 'test-token', {
    list: async () => { lists++; return { artifacts: [
      { id: 3, name, expired: true, workflow_run: { id: 30 } },
      { id: 2, name, expired: false, workflow_run: { id: 20 } },
      { id: 1, name, expired: false, workflow_run: { id: 10 } },
    ] }; },
    extract: async (artifact, destination) => {
      extracted.push(artifact.id);
      fs.mkdirSync(destination, { recursive: true });
      for (const asset of assets) fs.writeFileSync(path.join(destination, asset.name), artifact.id === 2 ? Buffer.alloc(bytes.length) : bytes);
    }, log: () => {},
  });
  assert.deepEqual(await source(assets[0]), bytes);
  assert.deepEqual(await source(assets[1]), bytes);
  assert.equal(lists, 1);
  assert.deepEqual(extracted, [2, 1]);
});

test('curl uploads authenticated multipart bytes, reports metrics and redacts HTTP errors', async () => {
  let status = 200;
  const asset = { ...release().assets[0], name: 'installer with spaces.msi' };
  const server = createServer(async (req, res) => {
    assert.equal(req.headers.authorization, undefined);
    assert.match(req.headers['content-type'], /^multipart\/form-data; boundary=/);
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = Buffer.concat(chunks).toString();
    assert.match(body, /filename="installer with spaces.msi"/);
    assert.match(body, /name="access_token"\r\n\r\nsecret-token/);
    assert.ok(body.includes(bytes.toString()));
    res.writeHead(status, { 'Content-Type': 'application/json' });
    res.end(status === 200 ? '{"id":123}' : 'secret-token private response');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const logs = [];
  const url = `http://127.0.0.1:${server.address().port}/upload`;
  try {
    assert.deepEqual(await uploadAttachment(url, 'secret-token', { asset, bytes }, { log: line => logs.push(line) }), { id: 123 });
    assert.ok(logs.some(line => /http_code=200; size_upload=\d+; speed_upload=/.test(line)));
    status = 401;
    await assert.rejects(uploadAttachment(url, 'secret-token', { asset, bytes }, { log: line => logs.push(line) }), error => {
      assert.match(error.message, /http_code=401/);
      assert.doesNotMatch(error.message, /secret-token|private response/);
      return true;
    });
    assert.doesNotMatch(logs.join('\n'), /secret-token|Authorization|private response/);
  } finally { await new Promise(resolve => server.close(resolve)); }
});

test('uses Gitee Bearer authentication and retains GitHub token authentication', async () => {
  const originalFetch = globalThis.fetch;
  const authorization = [];
  globalThis.fetch = async (_url, options) => {
    authorization.push(options.headers.Authorization);
    return { ok: true, status: 200, json: async () => ({}) };
  };
  try {
    await request('https://gitee.com/api/v5/user', { token: 'fixture-token' });
    await request('https://api.github.com/user', { token: 'fixture-token' });
    assert.deepEqual(authorization, ['Bearer fixture-token', 'token fixture-token']);
  } finally { globalThis.fetch = originalFetch; }
});

test('curl timeout reports actual bytes sent when the server withholds its response', async () => {
  let received = 0;
  const server = createServer(async req => {
    for await (const chunk of req) received += chunk.length;
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    await assert.rejects(uploadAttachment(`http://127.0.0.1:${server.address().port}/upload`, 'secret-token',
      { asset: release().assets[0], bytes }, { timeoutSeconds: 0.3, log: () => {} }), error => {
      assert.match(error.message, /curl_exit=28/);
      assert.match(error.message, /http_code=000/);
      assert.ok(Number(/size_upload=(\d+)/.exec(error.message)[1]) >= bytes.length);
      return true;
    });
    assert.ok(received >= bytes.length);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('reports nested transport failures without leaking credentials or arbitrary messages', async () => {
  const originalFetch = globalThis.fetch;
  const cause = Object.assign(new Error('secret-token in transport message'), {
    code: 'UND_ERR_HEADERS_TIMEOUT', headers: { Authorization: 'secret-token' },
  });
  cause.cause = cause; // Cycles must not break diagnostic reporting.
  globalThis.fetch = async () => { throw new TypeError('secret-token', { cause }); };
  try {
    await assert.rejects(request('https://example.com/upload?access_token=secret-token', {
      token: 'secret-token', method: 'POST', form: new FormData(),
    }), error => {
      assert.match(error.message, /phase=waiting-for-response-headers/);
      assert.match(error.message, /elapsed_ms=\d+; timeout_ms=3600000/);
      assert.match(error.message, /UND_ERR_HEADERS_TIMEOUT/);
      assert.doesNotMatch(error.message, /secret-token|access_token|Authorization/);
      return true;
    });
  } finally { globalThis.fetch = originalFetch; }
});

test('distinguishes response body failures and aggregate connection errors', async () => {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => ({
    status: 200, ok: true,
    json: async () => { throw Object.assign(new Error('private response'), { code: 'UND_ERR_BODY_TIMEOUT' }); },
  });
  try {
    await assert.rejects(request('https://example.com/api'), /HTTP 200; phase=reading-response-body;.*UND_ERR_BODY_TIMEOUT/);
    globalThis.fetch = async () => {
      throw new TypeError('private URL', { cause: new AggregateError([
        Object.assign(new Error('private address'), { code: 'ECONNREFUSED' }),
        Object.assign(new Error('private address'), { code: 'ENETUNREACH' }),
      ]) });
    };
    await assert.rejects(request('https://example.com/api'), error => {
      assert.match(error.message, /ECONNREFUSED/);
      assert.match(error.message, /ENETUNREACH/);
      assert.doesNotMatch(error.message, /private/);
      return true;
    });
  } finally { globalThis.fetch = originalFetch; }
});

test('does not mirror incomplete multi-platform releases', () => {
  const source = release();
  assert.deepEqual(validateRelease(source), []);
  source.assets.pop();
  assert.equal(validateRelease(source).length, 1);
  assert.throws(() => validateRelease({ ...release(), draft: true }));
});

test('rejects unverified metadata and unsafe URLs or names', () => {
  for (const change of [{ digest: null }, { size: 0 }, { name: '../escape.msi' },
    { browser_download_url: 'https://example.com/installer' }]) {
    const source = release();
    source.assets.push({ ...source.assets[0], ...change });
    assert.throws(() => validateRelease(source));
  }
  assert.throws(() => requiredAssets('--option'));
});

test('rejects same-size corrupted downloads and attachment conflicts', () => {
  const asset = release().assets[0];
  verifyAsset(asset, bytes);
  assert.throws(() => verifyAsset(asset, Buffer.alloc(bytes.length)));
  assert.throws(() => planAssets([asset], [{ name: asset.name, size: 1 }]));
  assert.throws(() => planAssets([asset], [asset, asset]));
});

test('resumes partial uploads without duplicate files and propagates edited notes', async () => {
  const source = release();
  const files = [];
  let target = null, fail = true, patches = 0, creates = 0;
  const api = async (method, suffix, data) => {
    if (method === 'GET') return target;
    if (method === 'POST') { creates++; target = { id: 123 }; return target; }
    if (method === 'LIST') return [...files];
    if (method === 'UPLOAD') {
      files.push({ name: data.asset.name, size: data.asset.size });
      // Simulate a lost upload response: server accepted the file before disconnecting.
      if (fail) { fail = false; throw new Error('Disconnected'); }
      return {};
    }
    if (method === 'PATCH') { assert.equal(data.body, 'Updated notes'); patches++; return target; }
    throw new Error(`Unexpected operation ${method} ${suffix}`);
  };
  const download = async asset => asset.name === 'torto-update.json' ? updateManifest(source).bytes : bytes;
  await assert.rejects(syncRelease(source, api, download));
  assert.equal(patches, 0);
  await syncRelease(source, api, download);
  await syncRelease(source, api, download);
  assert.equal(creates, 1);
  assert.equal(files.length, source.assets.length + 1);
  assert.equal(patches, 2);
});

test('does not mark a mirror verified when remote content is corrupt', async () => {
  const source = release();
  const api = async method => {
    if (method === 'GET') return { id: 1 };
    if (method === 'LIST') return source.assets;
    throw new Error('Must not update notes on failed verification');
  };
  await assert.rejects(syncRelease(source, api, async () => Buffer.alloc(bytes.length)), /verification failed/);
});
