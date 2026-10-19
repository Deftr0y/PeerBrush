import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import vm from 'node:vm';

const site = new URL('../public-site/', import.meta.url);
const read = name => fs.readFileSync(new URL(name, site), 'utf8');
const platform = await import(`data:text/javascript;base64,${Buffer.from(read('platform.js')).toString('base64')}`);
const feed = JSON.parse(read('downloads.json'));
const storeUrl = 'https://apps.microsoft.com/detail/9PNR6T1Q2ZL9';

function pendingWindows() {
  const fixture = structuredClone(feed);
  fixture.history[0].platforms = fixture.history[0].platforms.filter(p => p.id !== 'windows');
  fixture.windowsStore.version = '0.25';
  return fixture;
}

test('a pending latest Windows release keeps the older Store download available without changing history', () => {
  const fixture = pendingWindows();
  const snapshot = structuredClone(fixture);
  const downloads = platform.availableDownloads(fixture);
  const windows = downloads.find(p => p.id === 'windows');
  assert.equal(windows.url, storeUrl);
  assert.equal(windows.version, '0.25');
  assert.match(windows.releaseLabel, /0\.25/);
  assert.equal(windows.channel, 'microsoft-store');
  assert.deepEqual(downloads.filter(p => p.id !== 'windows').map(p => p.url), fixture.history[0].platforms.map(p => p.url));
  assert.deepEqual(fixture, snapshot);
});

test('Store availability survives an empty latest platform list', () => {
  const fixture = pendingWindows();
  fixture.history[0].platforms = [];
  const downloads = platform.availableDownloads(fixture);
  assert.equal(downloads.length, 1);
  assert.equal(downloads[0].url, storeUrl);
  assert.equal(downloads[0].version, '0.25');
});

test('without a valid Store channel, use an available older Windows package with its own version', () => {
  const fixture = pendingWindows();
  fixture.windowsStore.url = 'https://example.invalid/fake-store';
  const windows = platform.availableDownloads(fixture).find(p => p.id === 'windows');
  assert.ok(windows.url.includes('/v0.25/'));
  assert.equal(windows.version, 'v0.25');
  assert.equal(platform.safeAvailableDownload({ ...fixture.windowsStore, url: 'https://apps.microsoft.com.evil.invalid/detail/9PNR6T1Q2ZL9' }), false);
  assert.equal(platform.safeAvailableDownload({ ...fixture.windowsStore, url: 'https://apps.microsoft.com/detail/another-product' }), false);
  assert.equal(platform.safeAvailableDownload({ ...fixture.windowsStore, url: 'http://apps.microsoft.com/detail/9PNR6T1Q2ZL9' }), false);
  assert.equal(platform.safeDownloadUrl(storeUrl), false);
});

class Element {
  constructor() { this.children = []; this.attributes = {}; this.span = {}; }
  append(...items) { this.children.push(...items); }
  replaceChildren(...items) { this.children = items; }
  setAttribute(name, value) { this.attributes[name] = value; }
  querySelector() { return this.span; }
}

function homepage(detected, fixture = pendingWindows()) {
  const nodes = new Map();
  const document = {
    querySelector(key) { if (!nodes.has(key)) nodes.set(key, new Element()); return nodes.get(key); },
    createElement() { return new Element(); }, createTextNode(value) { return value; },
  };
  const main = read('main.js');
  const code = main.slice(main.indexOf('function renderDownloads(build'), main.indexOf('async function loadDownloads()'));
  const context = { document, detected, primaryDownload: document.querySelector('#primary-download'), otherDownloads: new Element(), platformList: new Element(), platformDescriptions: { windows: 'Windows', linux: 'Linux', macos: 'macOS' }, safeAvailableDownload: platform.safeAvailableDownload, makeIcon: () => new Element(), downloadLink: p => Object.assign(new Element(), { href: p.url }), URL };
  vm.createContext(context);
  vm.runInContext(code, context);
  const build = platform.latestRelease(fixture);
  assert.equal(context.renderDownloads(build, platform.availableDownloads(fixture)), true);
  return context;
}

test('Windows homepage primary button is a functional Store download labelled with the available version', () => {
  const context = homepage('windows');
  assert.equal(context.primaryDownload.href, storeUrl);
  assert.equal(context.primaryDownload.span.textContent, 'Download for Windows');
  assert.match(context.document.querySelector('#download-meta').textContent, /PeerBrush 0\.25 · Microsoft Store · Available now/);
  assert.doesNotMatch(context.document.querySelector('#download-meta').textContent, /NaN|0\.26/);
  assert.equal(context.platformList.children.length, 5);
  const row = context.platformList.children[0];
  assert.equal(row.children[1].textContent, 'Version 0.25');
  assert.equal(row.children[2].href, storeUrl);
});

test('Mac and Linux homepage downloads remain at their current exact URLs while Windows stays available', () => {
  for (const id of ['macos', 'linux']) {
    const context = homepage(id);
    const expected = platform.latestRelease(feed).platforms.find(p => p.id === id);
    assert.equal(context.primaryDownload.href, expected.url);
    assert.ok(context.otherDownloads.children.some(p => p.href === storeUrl));
  }
});

test('standalone Downloads and Windows page links use the same older Store selection', () => {
  const elements = new Map();
  for (const key of ['data-download-platform', 'data-platform-description', 'data-platform-size', 'data-platform-label']) elements.set(`[${key}="windows"]`, [new Element(), new Element()]);
  const document = { querySelectorAll: selector => elements.get(selector) || [] };
  const source = read('download-page.js');
  const code = source.slice(source.indexOf('export function renderPlatformLinks'), source.indexOf('\ntry {')).replace('export function', 'function');
  const context = { availableDownloads: platform.availableDownloads };
  vm.createContext(context);
  vm.runInContext(code, context);
  context.renderPlatformLinks(pendingWindows(), document);
  for (const link of elements.get('[data-download-platform="windows"]')) {
    assert.equal(link.href, storeUrl);
    assert.match(link.attributes['aria-label'], /0\.25/);
  }
  for (const size of elements.get('[data-platform-size="windows"]')) assert.equal(size.textContent, 'Version 0.25');
});

test('all three pages retain functional Windows links without JavaScript or feed access', () => {
  for (const page of ['index.html', 'downloads.html', 'windows.html']) {
    const html = read(page);
    assert.ok(html.includes(`href="${storeUrl}"`), page);
    assert.ok(html.includes(`data-download-platform="windows" href="${storeUrl}"`), page);
    assert.ok(html.includes('Version ' + feed.windowsStore.version), page);
    assert.ok(html.includes('Windows 0.26 update pending'), page);
    assert.equal(html.includes('disabled'), false, page);
    assert.equal(html.includes('from the same tested source'), false, page);
    const ids = new Set([...html.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]));
    for (const match of html.matchAll(/<use href="#([^"]+)"/g)) assert.ok(ids.has(match[1]), `${page}: missing SVG ${match[1]}`);
  }
});
