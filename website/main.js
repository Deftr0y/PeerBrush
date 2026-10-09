import { detectPlatform, completeRelease, safeDownloadUrl } from './platform.js';
import './showcase.js';

const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
const views = {
  workspace: { caption: 'Real workspace · Ember lake example · ImageGen artwork', alt: 'A mountain lake at sunset open in PeerBrush, with editable layers, masks and color effects.' },
  brush: { caption: 'Native workspace · Brush settings and stroke previews', alt: 'PeerBrush brush controls showing pressure, texture, taper and stroke settings.' },
  selection: { caption: 'Native workspace · Rectangular selection on the canvas', alt: 'A mountain lake in PeerBrush with a rectangular selection around the sun and distant mountains.' },
  color: { caption: 'Native workspace · Color picker and compact channel controls', alt: 'PeerBrush color picker with hue and saturation, channel controls and editable color values.' },
};
const tabs = [...document.querySelectorAll('[role="tab"]')];
const image = document.querySelector('#workspace-image');
const caption = document.querySelector('#screenshot-caption');
const panel = document.querySelector('#screenshot-panel');
const dialog = document.querySelector('.image-dialog');
const dialogImage = document.querySelector('#dialog-image');
let activeView = 'workspace';

function selectView(tab) {
  activeView = tab.dataset.view;
  for (const item of tabs) {
    item.setAttribute('aria-selected', String(item === tab));
    item.tabIndex = item === tab ? 0 : -1;
  }
  const view = views[activeView];
  image.src = `assets/${activeView}-v2.png`;
  if (!reducedMotion.matches) {
    image.getAnimations().forEach(animation => animation.cancel());
    image.animate([{ opacity: .35, transform: 'scale(1.015)' }, { opacity: 1, transform: 'scale(1)' }], { duration: 380, easing: 'ease-out' });
  }
  image.alt = view.alt;
  caption.textContent = view.caption;
  panel.setAttribute('aria-labelledby', tab.id);
  document.querySelector('.image-count').textContent = `${String(tabs.indexOf(tab) + 1).padStart(2, '0')} / 04`;
  for (const button of document.querySelectorAll('.screenshot-button, .expand-button')) {
    button.setAttribute('aria-label', `Enlarge ${activeView} screenshot`);
  }
}

const detected = detectPlatform({ platform: navigator.userAgentData?.platform || navigator.platform, userAgent: navigator.userAgent, maxTouchPoints: navigator.maxTouchPoints });
const platformDescriptions = { windows: 'x64 · Extract and run peerbrush.exe', linux: 'x64 · Built on Ubuntu 24.04', macos: 'Apple silicon · arm64 · Unsigned' };
const primaryDownload = document.querySelector('#primary-download');
const otherDownloads = document.querySelector('#other-downloads');
const platformList = document.querySelector('#platform-list');
function makeIcon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.classList.add('icon');
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#${name}`);
  svg.append(use);
  return svg;
}
function downloadLink(platform, className='text-link') {
  const link = document.createElement('a');
  link.href = platform.url;
  link.className = className;
  link.append(makeIcon(platform.id), document.createTextNode(platform.name));
  link.setAttribute('aria-label', `Download PeerBrush for ${platform.name}, ${platform.architecture}`);
  return link;
}
function renderDownloads(build) {
  if (!Array.isArray(build.platforms) || build.platforms.length !== 3 || build.platforms.some(p => !platformDescriptions[p.id] || !safeDownloadUrl(p.url))) return false;
  const platforms = [...build.platforms].sort((a,b) => Number(b.id === detected)-Number(a.id === detected));
  const suggested = platforms.find(p => p.id === detected);
  if (suggested) {
    primaryDownload.href = suggested.url;
    primaryDownload.querySelector('span').textContent = `Download for ${suggested.name}`;
    primaryDownload.setAttribute('aria-label', `Download PeerBrush for ${suggested.name}, ${suggested.architecture}`);
    document.querySelector('#download-meta').textContent = `${suggested.architecture === 'arm64' ? 'Apple silicon · ' : 'x64 · '}${(suggested.bytes/1e6).toFixed(1)} MB · Development build`;
  }
  otherDownloads.replaceChildren();
  for (const platform of platforms.filter(p => p.id !== detected)) otherDownloads.append(downloadLink(platform));
  platformList.replaceChildren();
  for (const platform of platforms) {
    const row = document.createElement('div');
    row.className = `platform-row${platform.id === detected ? ' recommended' : ''}`;
    const info = document.createElement('div');
    info.className = 'platform-info';
    info.append(makeIcon(platform.id));
    const copy = document.createElement('div');
    const heading = document.createElement('h3');
    heading.textContent = platform.name;
    if (platform.id === detected) {
      const hint = document.createElement('span');
      hint.className = 'os-hint';
      hint.textContent = 'Suggested for this device';
      heading.append(hint);
    }
    const detail = document.createElement('p');
    detail.textContent = platformDescriptions[platform.id];
    copy.append(heading, detail);
    info.append(copy);
    const size = document.createElement('span');
    size.className = 'package-size';
    size.textContent = `${(platform.bytes/1e6).toFixed(1)} MB`;
    const link = downloadLink(platform, 'platform-download');
    link.replaceChildren(document.createTextNode('Download'), makeIcon('download'));
    row.append(info, size, link);
    platformList.append(row);
  }
  document.querySelector('#build-label').textContent = build.label || build.version;
  try {
    const url = new URL(build.releaseUrl);
    if (url.hostname === 'github.com' && url.protocol === 'https:' && url.pathname.startsWith('/Deftr0y/PeerBrush/releases/tag/')) document.querySelector('#release-link').href = url.href;
  } catch { /* Keep the canonical release-list link. */ }
  return true;
}
async function loadDownloads() {
  // The bundled manifest remains usable if GitHub is unavailable or rate-limited.
  let loaded = false;
  try {
    const response = await fetch('downloads.json');
    if (response.ok) loaded = renderDownloads(await response.json());
  } catch { /* Try the public release feed below. */ }
  try {
    const response = await fetch('https://api.github.com/repos/Deftr0y/PeerBrush/releases?per_page=10', { signal: AbortSignal.timeout(5000), headers: { Accept: 'application/vnd.github+json' } });
    if (response.ok) {
      const releases = await response.json();
      const build = Array.isArray(releases) ? releases.map(completeRelease).find(Boolean) : null;
      if (build) loaded = renderDownloads(build);
    }
  } catch { /* The verified bundled release already supplies all three builds. */ }
  if (!loaded) document.querySelector('#build-label').textContent = 'Development · d70f64e';
}
loadDownloads();

const progress = document.querySelector('.reading-progress');
let scrollScheduled = false;
function updateProgress() {
  const range = document.documentElement.scrollHeight - window.innerHeight;
  progress.style.transform = `scaleX(${range > 0 ? window.scrollY / range : 0})`;
  scrollScheduled = false;
}
window.addEventListener('scroll', () => {
  if (!scrollScheduled) { scrollScheduled = true; requestAnimationFrame(updateProgress); }
}, { passive: true });
window.addEventListener('resize', updateProgress);
updateProgress();

if ('IntersectionObserver' in window && !reducedMotion.matches) {
  const observer = new IntersectionObserver(entries => {
    for (const entry of entries) {
      if (entry.isIntersecting) {
        entry.target.classList.add('revealed');
        observer.unobserve(entry.target);
      }
    }
  }, { threshold: .08 });
  for (const section of document.querySelectorAll('.section-heading, .section-intro, .steps, .capabilities article, .contribution-grid article, .community-links, .showcase-frame, .video-frame, .gallery, .platform-list, .format-table, .status-capture, .mcp-call, .community-heading, .shortcuts, .development')) {
    section.classList.add('reveal');
    observer.observe(section);
  }
}

for (const tab of tabs) {
  tab.addEventListener('click', () => selectView(tab));
  tab.addEventListener('keydown', event => {
    let index = tabs.indexOf(tab);
    if (event.key === 'ArrowRight') index = (index + 1) % tabs.length;
    else if (event.key === 'ArrowLeft') index = (index - 1 + tabs.length) % tabs.length;
    else if (event.key === 'Home') index = 0;
    else if (event.key === 'End') index = tabs.length - 1;
    else return;
    event.preventDefault();
    tabs[index].focus();
    selectView(tabs[index]);
  });
}

for (const button of document.querySelectorAll('.screenshot-button, .expand-button')) {
  button.addEventListener('click', () => {
    dialogImage.src = image.src;
    dialogImage.alt = image.alt;
    document.querySelector('#dialog-caption').textContent = views[activeView].caption;
    dialog.showModal();
  });
}
document.querySelector('.close-dialog').addEventListener('click', () => dialog.close());
dialog.addEventListener('click', event => {
  const bounds = dialog.getBoundingClientRect();
  if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) dialog.close();
});

for (const button of document.querySelectorAll('[data-copy]')) {
  button.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(button.dataset.copy);
      button.textContent = 'Copied';
      document.querySelector('#status').textContent = 'Development commands copied.';
      window.setTimeout(() => { button.textContent = 'Copy'; }, 2000);
    } catch {
      button.textContent = 'Select text';
      const range = document.createRange();
      range.selectNodeContents(button.parentElement.querySelector('code'));
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.querySelector('#status').textContent = 'Clipboard unavailable. Commands selected; use your copy shortcut.';
    }
  });
}
