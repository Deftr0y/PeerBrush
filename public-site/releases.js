import { releaseHistory, latestRelease, safeDownloadUrl } from './platform.js';

function historyDownloadLink(platform) {
  const link = document.createElement('a');
  link.href = platform.url;
  link.className = 'text-link';
  link.textContent = platform.name;
  link.setAttribute('aria-label', `Download PeerBrush for ${platform.name}, ${platform.architecture}`);
  return link;
}

export function renderHistory(data) {
  const list=document.querySelector('#release-history');
  if (!list) return;
  const latest = latestRelease(data);
  const latestOnly = list.dataset.scope === 'latest';
  const releases = latestOnly ? (latest ? [latest] : []) : releaseHistory(data);
  list.replaceChildren();
  for(const release of releases) {
    const item=document.createElement('li');
    const heading=document.createElement('h3');
    const link=document.createElement('a');link.href=release.releaseUrl;link.textContent=release.label || release.version;heading.append(link);
    const date=document.createElement('p');date.className='history-date';date.textContent=`${release.publishedAt.slice(0,10)} · ${release.kind} · ${release.commit.slice(0,7)}`;
    const notes=document.createElement('ul');
    for(const note of release.notes) {const line=document.createElement('li');line.textContent=note;notes.append(line);}
    item.append(heading,date,notes);
    list.append(item);
    if (latestOnly) continue;
    const downloads=document.createElement('div');downloads.className='history-downloads';
    for(const platform of release.platforms) {
      const row=document.createElement('p');row.append(historyDownloadLink(platform),document.createTextNode(` · ${platform.architecture}`));
      if(platform.sha256) {const code=document.createElement('code');code.textContent=platform.sha256;row.append(document.createTextNode(' · SHA-256 '),code);}
      else row.append(document.createTextNode(' · Checksum not recorded'));
      downloads.append(row);
    }
    const missing=['Windows','Linux','macOS'].filter(name=>!release.platforms.some(p=>p.name===name));
    if(missing.length) {const line=document.createElement('p');line.textContent=`No published ${missing.join(' / ')} package for this checkpoint.`;downloads.append(line);}
    const docs = document.createElement('a');
    docs.className = 'text-link';
    const documentationRef = /^[0-9a-f]{40}$/.test(release.packagingCommit || '') ? release.packagingCommit : release.version;
    docs.href = `https://github.com/Deftr0y/PeerBrush/tree/${encodeURIComponent(documentationRef)}/docs`;
    docs.textContent = 'Documentation for this version';
    for (const [label,url] of [['Matching source',release.sourceUrl],['Checksums',release.checksumsUrl]]) {
      if (!safeDownloadUrl(url)) continue;
      const row=document.createElement('p');const link=document.createElement('a');
      link.className='text-link';link.href=url;link.textContent=label;row.append(link);downloads.append(row);
    }
    item.append(downloads,docs);
  }
}
