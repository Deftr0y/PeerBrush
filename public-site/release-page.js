import { renderHistory } from './releases.js';
import { latestRelease, releaseHistory } from './platform.js';

const status = document.querySelector('#history-status');
try {
  const feedUrl = new URL('./downloads.json', import.meta.url);
  feedUrl.search = new URL(import.meta.url).search;
  const response = await fetch(feedUrl, { cache: 'no-store' });
  if (!response.ok) throw new Error('Release history unavailable');
  const data = await response.json();
  if (!releaseHistory(data).length) throw new Error('No published releases');
  renderHistory(data);
  status.hidden = true;
  const latest = latestRelease(data);
  if (latest) {
    for (const link of document.querySelectorAll('[data-doc]')) {
      link.href = `https://github.com/Deftr0y/PeerBrush/blob/${encodeURIComponent(latest.version)}/docs/${link.dataset.doc}`;
    }
  }
} catch {
  status.textContent = 'Release history is unavailable. ';
  const link = document.createElement('a');
  link.className = 'text-link';
  link.href = 'https://github.com/Deftr0y/PeerBrush/releases';
  link.textContent = 'Read all release notes on GitHub';
  status.append(link);
}
