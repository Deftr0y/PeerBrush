(() => {
  // Public site identifier; no API key or private account credentials are needed.
  const endpoint = 'https://peerbrush.goatcounter.com/count';
  if (!endpoint || location.protocol !== 'https:' ||
      !['peerbrush.com', 'www.peerbrush.com'].includes(location.hostname) ||
      navigator.doNotTrack === '1' || navigator.globalPrivacyControl === true ||
      window.goatcounter) return;

  // Only count known public pages; never include query strings or fragments.
  const paths = {
    '/': '/',
    '/index.html': '/',
    '/releases.html': '/releases.html',
    '/privacy.html': '/privacy.html',
    '/community.html': '/community.html',
  };
  const path = paths[location.pathname];
  if (!path) return;

  // Keep the referring site's origin, without its path, query or fragment.
  let referrer = '';
  try {
    const url = new URL(document.referrer);
    if (['http:', 'https:'].includes(url.protocol) &&
        !['peerbrush.com', 'www.peerbrush.com'].includes(url.hostname)) {
      referrer = url.origin;
    }
  } catch { /* Direct visits have no referrer. */ }

  window.goatcounter = { path, referrer, no_events: true, no_onload: true };
  const script = document.createElement('script');
  script.async = true;
  script.src = 'https://gc.zgo.at/count.v5.js';
  script.crossOrigin = 'anonymous';
  script.integrity = 'sha384-atnOLvQb9t+jTSipvd75X2yginT4PjVbqDdlJAmxMm+wYElFmeR6EmLP5bYeoRVQ';
  script.referrerPolicy = 'no-referrer';
  script.setAttribute('data-goatcounter', endpoint);
  script.onload = () => {
    const counter = window.goatcounter;
    // GoatCounter also sends location.search separately as campaign data (q).
    // Remove it before allowing the first or any subsequent count request.
    const getData = counter.get_data;
    counter.get_data = (vars) => {
      const data = getData(vars);
      delete data.q;
      return data;
    };
    const countWhenVisible = () => {
      if (document.visibilityState === 'hidden') return;
      document.removeEventListener('visibilitychange', countWhenVisible);
      counter.count();
    };
    if (document.visibilityState === 'hidden') {
      document.addEventListener('visibilitychange', countWhenVisible);
    } else {
      countWhenVisible();
    }
  };
  document.head.appendChild(script);
})();
