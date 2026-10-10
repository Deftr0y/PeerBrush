import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

const root = new URL('../', import.meta.url);
test('trailer uses accessible, user-controlled inline playback', async () => {
  const html = await readFile(new URL('index.html', root), 'utf8');
  const video = html.match(/<video\b[^>]*>/)?.[0];
  assert.ok(video);
  for (const attribute of ['controls', 'playsinline', 'preload="metadata"', 'aria-labelledby="trailer-title"']) {
    assert.ok(video.includes(attribute), attribute);
  }
  assert.doesNotMatch(video, /\b(?:autoplay|loop)\b/);
  assert.match(html, /<source src="assets\/peerbrush-trailer-v2\.1\.mp4" type="video\/mp4">/);
  assert.match(html, /id="trailer-title"/);
  assert.ok(video.includes('controlslist="nodownload"'));
});
test('build includes the exact canonical trailer bytes', async () => {
  execFileSync(process.execPath, ['build.mjs'], { cwd: root });
  const source = await readFile(new URL('public/assets/peerbrush-trailer-v2.1.mp4', root));
  const built = await readFile(new URL('dist/assets/peerbrush-trailer-v2.1.mp4', root));
  const hash = bytes => createHash('sha256').update(bytes).digest('hex');
  assert.equal(hash(source), 'da8917900c1ca2718dffd2e2ca938a660d2debfb51b73ea392887f2b333d39b6');
  assert.equal(hash(built), hash(source));
  assert.ok(built.length > 0);
});
