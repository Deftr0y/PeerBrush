import { availableDownloads } from './platform.js?release=v0.26.0&downloads=2';

export function renderPlatformLinks(data, document = globalThis.document) {
  for (const platform of availableDownloads(data)) {
    for (const link of document.querySelectorAll(`[data-download-platform="${platform.id}"]`)) {
      link.href = platform.url;
      link.setAttribute('aria-label', `Download PeerBrush for ${platform.name}, ${platform.architecture}, ${platform.releaseLabel}`);
    }
    for (const element of document.querySelectorAll(`[data-platform-description="${platform.id}"]`)) element.textContent = platform.description;
    for (const element of document.querySelectorAll(`[data-platform-size="${platform.id}"]`)) element.textContent = platform.channel === 'microsoft-store' ? 'Version ' + platform.version : `${(platform.bytes / 1e6).toFixed(1)} MB`;
    for (const element of document.querySelectorAll(`[data-platform-label="${platform.id}"]`)) element.textContent = platform.releaseLabel;
  }
}

try {
  const response = await fetch('./downloads.json?release=v0.26.0&downloads=2', { cache: 'no-store' });
  if (response.ok) renderPlatformLinks(await response.json());
} catch { /* Keep functional static downloads when the feed is unavailable. */ }
