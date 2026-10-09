import test from 'node:test';
import assert from 'node:assert/strict';
import { detectPlatform, completeRelease, safeDownloadUrl } from '../platform.js';

test('suggests desktop builds and avoids incompatible mobile devices', () => {
  for (const [input, expected] of [
    [{ platform: 'Win32' }, 'windows'],
    [{ platform: 'Windows', userAgent: 'Mozilla/5.0' }, 'windows'],
    [{ platform: 'MacIntel' }, 'macos'],
    [{ userAgent: 'X11; Linux x86_64' }, 'linux'],
    [{ userAgent: 'Android Linux' }, null],
    [{ platform: 'MacIntel', maxTouchPoints: 5 }, null],
    [{ userAgent: 'iPhone' }, null],
    [{ userAgent: 'CrOS x86_64' }, null],
    [{ platform: 'Darwin' }, null],
    [{}, null],
  ]) assert.equal(detectPlatform(input), expected);
});

test('a single release must supply all platforms with canonical download URLs', () => {
  const assets = ['Windows', 'Linux', 'macOS'].map(name => ({ name: `PeerBrush-${name}.zip`, browser_download_url: `https://github.com/Deftr0y/PeerBrush/releases/download/test/PeerBrush-${name}.zip`, size: 100 }));
  const release = { tag_name: 'test', html_url: 'https://github.com/Deftr0y/PeerBrush/releases/tag/test', prerelease: true, assets };
  assert.equal(completeRelease(release).platforms.length, 3);
  assert.equal(completeRelease({ ...release, assets: assets.slice(0, 2) }), null);
  assert.equal(completeRelease({ ...release, draft: true }), null);
  assert.equal(completeRelease({ ...release, assets: assets.map(a => ({ ...a, browser_download_url: 'https://example.com/package.zip' })) }), null);
  assert.equal(safeDownloadUrl('https://github.com.evil.example/Deftr0y/PeerBrush/releases/download/a/b'), false);
  assert.equal(safeDownloadUrl('https://user:password@github.com/Deftr0y/PeerBrush/releases/download/a/b'), false);
});
