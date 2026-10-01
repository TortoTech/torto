import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { requiredAssets, validateRelease, verifyAsset, planAssets, syncRelease } from './sync-gitee-release.mjs';

const bytes = Buffer.from('installer fixture');
const digest = `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
const release = () => ({
  tag_name: 'v0.7.0', name: 'Torto 0.7.0', body: 'Updated notes', draft: false, prerelease: false,
  assets: requiredAssets('v0.7.0').map(name => ({ name, size: bytes.length, digest,
    browser_download_url: `https://github.com/TortoTech/torto/releases/download/v0.7.0/${name}` })),
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
  await assert.rejects(syncRelease(source, api, async () => bytes));
  assert.equal(patches, 0);
  await syncRelease(source, api, async () => bytes);
  await syncRelease(source, api, async () => bytes);
  assert.equal(creates, 1);
  assert.equal(files.length, source.assets.length);
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
