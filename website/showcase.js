// Visual layer of the website: hero parallax, the tilting workspace
// window, the trailer play button, the scroll-driven layer breakdown of the
// golden-hour example and card spotlights.
// Everything degrades to static, fully usable content with reduced motion.

const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
const narrow = window.matchMedia('(max-width: 900px)');
const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const smooth = (a, b, v) => { const x = clamp((v - a) / (b - a)); return x * x * (3 - 2 * x); };

/* ------------------------------------------------------------ header + hero */
const headerShell = document.querySelector('.header-shell');
const hero = document.querySelector('.hero');
const heroImage = document.querySelector('.hero-image');
const showcaseTilt = document.querySelector('.showcase-tilt');

function updateHero() {
  const y = window.scrollY;
  headerShell?.classList.toggle('scrolled', y > 24);
  if (reduced.matches) return;
  if (hero && heroImage && y < hero.offsetHeight) {
    heroImage.style.setProperty('--hero-zoom', (y / hero.offsetHeight).toFixed(4));
    heroImage.style.setProperty('--hero-shift', (y * 0.22).toFixed(1));
  }
  if (showcaseTilt) {
    const top = showcaseTilt.getBoundingClientRect().top;
    showcaseTilt.style.setProperty('--tilt', smooth(window.innerHeight * 0.18, window.innerHeight * 0.85, top).toFixed(4));
  }
}

/* ------------------------------------------------------------ trailer play button */
function setupTrailer() {
  const video = document.querySelector('.trailer-video');
  const overlay = document.querySelector('.play-overlay');
  if (!video || !overlay) return;
  overlay.addEventListener('click', () => { overlay.classList.add('hidden'); video.play().catch(() => overlay.classList.remove('hidden')); video.focus({ preventScroll: true }); });
  video.addEventListener('play', () => overlay.classList.add('hidden'));
  video.addEventListener('ended', () => overlay.classList.remove('hidden'));
}

/* ------------------------------------------------------------ layer breakdown */
function setupLayers() {
  const section = document.querySelector('.layers-story');
  if (!section) return null;
  const stack = section.querySelector('.stack');
  const flat = section.querySelector('.stack-flat');
  const planes = [...section.querySelectorAll('.plane')];
  const labels = [...section.querySelectorAll('.layer-label')];
  const steps = [...section.querySelectorAll('.layers-steps li')];
  const links = section.querySelector('.layer-links');
  const status = document.querySelector('#status');
  const planeFor = (id) => planes.find(p => p.dataset.layer === id);
  const lines = labels.map(() => { const l = document.createElementNS('http://www.w3.org/2000/svg', 'line'); links.append(l); return l; });

  for (const label of labels) {
    label.addEventListener('click', () => {
      const on = label.getAttribute('aria-pressed') !== 'true';
      label.setAttribute('aria-pressed', String(on));
      planeFor(label.dataset.layer)?.classList.toggle('off', !on);
      const name = [...label.childNodes].filter(n => n.nodeType === Node.TEXT_NODE).map(n => n.textContent).join('').trim();
      status.textContent = `${name} layer (${label.querySelector('.who').textContent}) ${on ? 'shown' : 'hidden'}.`;
    });
  }

  const regions = section.querySelector('.regions');

  function staticMode() {
    stack.classList.add('flat');
    stack.style.setProperty('--t', 0);
    flat.style.opacity = 0;
    regions?.style.setProperty('--regions', 1);
    regions?.style.setProperty('--areas', 1);
    for (const p of planes) { p.style.setProperty('--z', 0); p.style.setProperty('--slab', 0); }
    for (const l of labels) { l.style.setProperty('--labels', 1); l.style.pointerEvents = ''; }
    steps.forEach(s => s.classList.add('active'));
    lines.forEach(l => l.setAttribute('opacity', 0));
  }

  function update() {
    if (reduced.matches || narrow.matches) { staticMode(); return; }
    const rect = section.getBoundingClientRect();
    const p = clamp(-rect.top / (rect.height - window.innerHeight));
    // 0: both editing at once · 1: different parts of the canvas · 2: different layers · 3: still editable
    const t = smooth(.33, .52, p) * (1 - smooth(.82, .93, p));
    const s = smooth(.36, .58, p) * (1 - smooth(.80, .91, p));
    const lab = smooth(.54, .62, p);
    stack.style.setProperty('--t', t.toFixed(4));
    stack.classList.toggle('flat', t < .001 && s < .001);
    flat.style.opacity = (1 - smooth(.33, .38, p)).toFixed(3);
    regions?.style.setProperty('--regions', (smooth(.0, .05, p) * (1 - smooth(.30, .35, p))).toFixed(3));
    regions?.style.setProperty('--areas', smooth(.14, .2, p).toFixed(3));
    const spacing = stack.clientWidth * .085;
    planes.forEach((plane, i) => { plane.style.setProperty('--z', ((i - 3) * spacing * s).toFixed(1)); plane.style.setProperty('--slab', s.toFixed(3)); });
    for (const l of labels) { l.style.setProperty('--labels', lab.toFixed(3)); l.style.pointerEvents = lab > .5 ? '' : 'none'; }
    const step = p < .15 ? 0 : p < .34 ? 1 : p < .80 ? 2 : 3;
    steps.forEach((li, i) => li.classList.toggle('active', i === step));
    const box = links.getBoundingClientRect();
    labels.forEach((label, i) => {
      const anchor = planeFor(label.dataset.layer)?.querySelector('.anchor');
      const line = lines[i];
      const show = lab * smooth(.45, .9, s);
      if (!anchor || show < .02) { line.setAttribute('opacity', 0); return; }
      const a = anchor.getBoundingClientRect(), b = label.getBoundingClientRect();
      line.setAttribute('x1', (a.left - box.left).toFixed(1)); line.setAttribute('y1', (a.top - box.top).toFixed(1));
      line.setAttribute('x2', (b.left - box.left - 4).toFixed(1)); line.setAttribute('y2', (b.top + b.height / 2 - box.top).toFixed(1));
      line.setAttribute('stroke', label.classList.contains('artist') ? '#e95420' : '#5aaaff');
      line.setAttribute('opacity', (show * .8).toFixed(3));
    });
  }
  return update;
}

/* ------------------------------------------------------------ card spotlight */
function setupSpotlights() {
  for (const card of document.querySelectorAll('.spot')) {
    card.addEventListener('pointermove', (e) => {
      const r = card.getBoundingClientRect();
      card.style.setProperty('--mx', `${e.clientX - r.left}px`);
      card.style.setProperty('--my', `${e.clientY - r.top}px`);
    });
  }
}

/* ------------------------------------------------------------ wiring */
const updateLayers = setupLayers();
let scheduled = false;
function onScroll() {
  if (scheduled) return;
  scheduled = true;
  requestAnimationFrame(() => { scheduled = false; updateHero(); updateLayers?.(); });
}
window.addEventListener('scroll', onScroll, { passive: true });
window.addEventListener('resize', onScroll);
reduced.addEventListener?.('change', onScroll);
narrow.addEventListener?.('change', onScroll);
for (const img of document.querySelectorAll('.layers-story img')) img.addEventListener('load', onScroll, { once: true });
setupTrailer();
setupSpotlights();
onScroll();
