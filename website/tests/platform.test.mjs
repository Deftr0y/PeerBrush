import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { detectPlatform, completeRelease, safeDownloadUrl, releaseHistory, latestRelease } from '../platform.js';

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

test('latest selection stays on the explicitly published entry and preserves incomplete history', () => {
  const platforms=['windows','linux','macos'].map(id=>({id,name:id,architecture:id==='macos'?'arm64':'x64',url:`https://github.com/Deftr0y/PeerBrush/releases/download/selected/${id}.zip`,bytes:100,sha256:'a'.repeat(64)}));
  const entry={version:'selected',kind:'development',publishedAt:'2026-10-09T14:57:54Z',commit:'a'.repeat(40),releaseUrl:'https://github.com/Deftr0y/PeerBrush/releases/tag/selected',notes:['Verified changes'],platforms};
  const old={...entry,version:'older',platforms:platforms.slice(0,1)};
  const future={...entry,version:'future'};
  const data={version:'selected',history:[future,entry,old]};
  assert.equal(latestRelease(data),entry);
  assert.equal(releaseHistory(data).length,3);
  assert.equal(latestRelease({...data,version:'older'}),null);
  assert.equal(latestRelease({...data,version:'unpublished'}),null);
  assert.equal(releaseHistory({...data,history:[{...entry,commit:'main'}]}).length,0);
  assert.equal(releaseHistory({...data,history:[{...entry,platforms:[...platforms,platforms[0]]}]}).length,0);
  assert.equal(releaseHistory({...data,history:[{...entry,platforms:platforms.map(p=>({...p,sha256:'invalid'}))}]}).length,0);
});

test('the bundled manifest selects a complete published release and preserves its history', async () => {
  const data=JSON.parse(await readFile(new URL('../public/downloads.json',import.meta.url),'utf8'));
  const history=releaseHistory(data);
  assert.equal(history.length,data.history.length);
  assert.equal(new Set(history.map(r=>r.version)).size,history.length);
  assert.equal(latestRelease(data)?.version,data.version);
  assert.equal(latestRelease(data)?.platforms.length,3);
  for(const release of history) {
    assert.equal(decodeURIComponent(new URL(release.releaseUrl).pathname.split('/').at(-1)),release.version);
    for(const platform of release.platforms) {
      assert.equal(decodeURIComponent(new URL(platform.url).pathname.split('/').at(-2)),release.version);
    }
  }
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
