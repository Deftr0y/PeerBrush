const $ = id => document.getElementById(id);
const number = value => Number.isFinite(value) ? value.toLocaleString('en-US') : '-';
const formatTime = value => value && !Number.isNaN(Date.parse(value)) ? new Date(value).toLocaleString('en-GB', { timeZone: 'UTC', dateStyle: 'medium', timeStyle: 'short' }) + ' UTC' : 'No successful read';
const formatDay = value => new Date(value).toLocaleDateString('en-GB', { timeZone: 'UTC', day: 'numeric', month: 'short' });
const pageNames = { '/': 'Home', '/releases.html': 'Release notes & docs', '/privacy.html': 'Privacy', '/community.html': 'Community' };
const platformNames = { windows: 'Windows  -  GitHub packages', macos: 'macOS', linux: 'Linux' };
let selectedDays = 30;
let currentRequest;
let lastData;

function node(tag, text, className) {
  const element = document.createElement(tag);
  if (text !== undefined) element.textContent = text;
  if (className) element.className = className;
  return element;
}
function svgNode(tag, attributes, text) {
  const element = document.createElementNS('http://www.w3.org/2000/svg', tag);
  for (const [key, value] of Object.entries(attributes || {})) element.setAttribute(key, value);
  if (text !== undefined) element.textContent = text;
  return element;
}
function empty(id, message) { $(id).replaceChildren(node('p', message, 'empty')); }
function ranking(id, result, missing, names = {}) {
  if (!result) { empty(id, missing); return; }
  if (!result.rows.length) { empty(id, 'No results reported for this period.'); return; }
  const maximum = Math.max(1, ...result.rows.map(row => row.count));
  const list = node('ol', undefined, 'rank-list');
  for (const row of result.rows) {
    const li = node('li');
    li.append(node('span', names[row.name] || row.name, 'rank-name'), node('span', number(row.count), 'rank-count'));
    const svg = svgNode('svg', { viewBox: '0 0 1000 4', preserveAspectRatio: 'none', 'aria-hidden': 'true' });
    svg.append(svgNode('rect', { width: 1000, height: 4, rx: 2, class: 'rank-background' }), svgNode('rect', { width: Math.round(row.count / maximum * 1000), height: 4, rx: 2, class: 'rank-value' }));
    li.append(svg); list.append(li);
  }
  $(id).replaceChildren(list, node('p', result.more ? 'Top 10 results. More results exist at the source. Bars scale to the largest result.' : 'Bars scale to the largest result.', 'rank-caption'));
}
function chart(series) {
  if (!series) { empty('traffic-chart', 'Traffic is unavailable until GoatCounter can be read.'); $('daily-details').hidden = true; $('traffic-peak').textContent = ''; return; }
  if (!series.length) { empty('traffic-chart', 'No daily statistics were returned.'); $('daily-details').hidden = true; return; }
  const width = Math.max(320, $('traffic-chart').clientWidth);
  const height = 240, left = 60, right = width - 14, top = 16, bottom = height - 34;
  const peak = Math.max(...series.map(row => row.count)), maximum = Math.max(1, peak);
  const step = 10 ** Math.floor(Math.log10(maximum));
  const limit = Math.ceil(maximum / step) * step;
  const svg = svgNode('svg', { viewBox: `0 0 ${width} ${height}`, role: 'img', 'aria-labelledby': 'chart-title chart-description' });
  svg.append(svgNode('title', { id: 'chart-title' }, 'Daily counted website visits'), svgNode('desc', { id: 'chart-description' }, `${formatDay(series[0].date)} to ${formatDay(series.at(-1).date)}. Peak ${number(peak)} counted visits in one day. Exact values are in the Daily values table below.`));
  for (const value of [0, limit / 2, limit]) {
    const y = bottom - value / limit * (bottom - top);
    svg.append(svgNode('line', { x1: left, x2: right, y1: y, y2: y, class: 'grid-line' }), svgNode('text', { x: left - 10, y: y + 4, 'text-anchor': 'end' }, number(value)));
  }
  const coordinates = series.map((row, i) => [left + i / Math.max(1, series.length - 1) * (right - left), bottom - row.count / limit * (bottom - top)]);
  svg.append(svgNode('polygon', { points: `${left},${bottom} ${coordinates.map(pair => pair.join(',')).join(' ')} ${right},${bottom}`, class: 'area' }), svgNode('polyline', { points: coordinates.map(pair => pair.join(',')).join(' '), class: 'trend-line' }));
  for (const i of [0, Math.floor(series.length / 2), series.length - 1]) svg.append(svgNode('text', { x: coordinates[i][0], y: height - 7, 'text-anchor': i === 0 ? 'start' : i === series.length - 1 ? 'end' : 'middle' }, formatDay(series[i].date)));
  $('traffic-chart').replaceChildren(svg);
  $('traffic-peak').textContent = `Daily peak ${number(peak)}`;
  $('daily-values').replaceChildren(...series.map(row => { const tr = node('tr'); tr.append(node('td', row.date), node('td', number(row.count))); return tr; }));
  $('daily-details').hidden = false;
}
function releaseRows(releases) {
  const target = $('release-rows');
  if (!releases || !releases.length) {
    const tr = node('tr'), td = node('td', releases ? 'No published releases reported.' : 'GitHub release statistics are unavailable.');
    td.colSpan = 6; tr.append(td); target.replaceChildren(tr); return;
  }
  target.replaceChildren(...releases.map(release => {
    const tr = node('tr'), title = node('td');
    title.append(node('span', release.version, 'release-tag'), node('span', release.kind, 'release-kind'));
    tr.append(title, node('td', formatDay(release.publishedAt) + ' ' + new Date(release.publishedAt).getUTCFullYear()));
    for (const platform of ['windows', 'macos', 'linux']) tr.append(node('td', release.assets.some(asset => asset.platform === platform) ? number(release.platforms[platform]) : '-'));
    tr.append(node('td', number(release.requests))); return tr;
  }));
}
function health(data) {
  const names = { ready: 'Connected', partial: 'Partial data', missing: 'Not connected', error: 'Unavailable' };
  $('source-health').replaceChildren(...[['GoatCounter', data.traffic], ['GitHub', data.github], ['Microsoft Store', data.store]].map(([name, source]) => {
    const article = node('article', undefined, source.status);
    article.append(node('strong', name), node('p', data.mode === 'demo' && source.status === 'ready' ? 'Synthetic demo' : names[source.status], 'state'), node('p', formatTime(source.updatedAt)));
    return article;
  }));
  $('coverage').replaceChildren(...data.coverage.map(item => node('li', item)));
  $('source-notices').replaceChildren(...[...data.traffic.notices.map(n => `GoatCounter: ${n}`), ...data.github.notices.map(n => `GitHub: ${n}`)].map(n => node('p', n)));
}
function render(data) {
  const t = data.traffic, g = data.github;
  $('demo-banner').hidden = data.mode !== 'demo';
  $('period-label').textContent = `${formatDay(data.period.start)} - ${formatDay(new Date(Date.parse(data.period.end) - 86400000).toISOString())}  -  complete UTC days`;
  $('visits').textContent = number(t.visits); $('requests').textContent = number(g.requests); $('stars').textContent = number(g.stars);
  $('acquisitions').textContent = number(data.store.acquisitions);
  let change = 'GoatCounter  -  selected period';
  if (t.visits !== null && t.previousVisits !== null) {
    const delta = t.visits - t.previousVisits;
    change = t.previousVisits ? `${delta > 0 ? '+' : ''}${(delta / t.previousVisits * 100).toFixed(1)}% vs previous ${data.period.days} days` : 'No visits in previous period';
  } else if (t.visits === null) change = 'Traffic source unavailable';
  $('visit-change').textContent = change;
  $('traffic-source').textContent = `GoatCounter  -  ${formatTime(t.updatedAt)}`;
  $('github-source').textContent = `GitHub  -  ${formatTime(g.updatedAt)}`;
  chart(t.trend);
  for (const id of ['countries', 'referrers', 'pages', 'systems', 'browsers']) ranking(id, t[id], 'GoatCounter data is not available.', id === 'pages' ? pageNames : {});
  ranking('platforms', g.platforms ? { rows: g.platforms.map(row => ({ name: row.id, count: row.requests })), more: false } : null, 'GitHub package statistics are unavailable.', platformNames);
  $('store-notice').textContent = data.store.notice;
  releaseRows(g.releases);
  $('forks').textContent = number(g.forks); $('issues').textContent = number(g.openIssuesAndPRs); $('release-count').textContent = g.releases ? number(g.releases.length) : '-';
  health(data);
}
function clear() {
  lastData = null;
  for (const id of ['visits', 'requests', 'stars', 'acquisitions', 'forks', 'issues', 'release-count']) $(id).textContent = '-';
  for (const id of ['countries', 'referrers', 'pages', 'systems', 'browsers', 'platforms']) empty(id, 'Waiting for statistics.');
  chart(null); releaseRows(null); $('source-health').replaceChildren(); $('coverage').replaceChildren(); $('source-notices').replaceChildren();
  $('traffic-source').textContent = 'GoatCounter  -  awaiting source'; $('github-source').textContent = 'GitHub  -  awaiting source'; $('visit-change').textContent = 'GoatCounter  -  selected period';
}
async function load() {
  currentRequest?.abort(); const controller = new AbortController(); currentRequest = controller;
  clear(); $('main').setAttribute('aria-busy', 'true'); $('refresh').disabled = true;
  $('status').className = 'load-status'; $('status').textContent = 'Reading available sources.';
  try {
    const response = await fetch(`/stats/data/${selectedDays}.json`, { cache: 'no-store', credentials: 'same-origin', redirect: 'error', signal: controller.signal });
    
    if (!response.ok || !response.headers.get('content-type')?.includes('application/json')) throw new Error('Statistics could not be loaded. Please try again later.');
    const data = await response.json(); if (data.schema !== 1) throw new Error('The dashboard received an unsupported data format.');
    if (currentRequest !== controller || controller.signal.aborted) return;
    lastData = data; render(data);
    const incomplete = [data.traffic, data.github].some(source => source.status !== 'ready');
    const age = Date.now() - Date.parse(data.generatedAt);
    const freshness = age > 86400000 ? ' This snapshot is over 24 hours old.' : ''; 
    $('status').textContent = `Snapshot published ${formatTime(data.generatedAt)}.${incomplete ? ' Some sources are unavailable; see coverage below.' : ''}${freshness} Reload checks for a newer published snapshot.`;
  } catch (error) {
    if (error.name === 'AbortError') return;
    $('status').className = 'load-status error'; $('status').textContent = error.message;
    for (const id of ['countries', 'referrers', 'pages', 'systems', 'browsers', 'platforms']) empty(id, 'Statistics could not be loaded. Please try again later.');
  } finally {
    if (currentRequest === controller) { $('main').removeAttribute('aria-busy'); $('refresh').disabled = false; }
  }
}
document.querySelectorAll('[data-days]').forEach(button => button.addEventListener('click', () => {
  selectedDays = Number(button.dataset.days);
  document.querySelectorAll('[data-days]').forEach(b => b.setAttribute('aria-pressed', String(b === button))); load();
}));
$('refresh').addEventListener('click', load);
window.addEventListener('resize', () => { if (lastData) chart(lastData.traffic.trend); });
load();
