const colors = { windows: '#ff7a45', macos: '#5aaaff', linux: '#bd95cb' };
const names = { windows: 'Windows', macos: 'macOS', linux: 'Linux' };
const fmt = n => n.toLocaleString('en-US', { maximumFractionDigits: 1 });
const day = date => new Date(`${date.slice(0, 10)}T12:00:00Z`).toLocaleDateString('en-GB', { timeZone: 'UTC', day: 'numeric', month: 'short', year: 'numeric' });
const el = (tag, text, className) => { const e = document.createElement(tag); if (text !== undefined) e.textContent = text; if (className) e.className = className; return e; };
const svg = (tag, attrs, text) => { const e = document.createElementNS('http://www.w3.org/2000/svg', tag); for (const [k,v] of Object.entries(attrs || {})) e.setAttribute(k,v); if (text !== undefined) e.textContent = text; return e; };

export function movingAverage(series, window = 7) {
  return series.map((row, i) => ({ date: row.date, count: i + 1 < window ? null : series.slice(i + 1 - window, i + 1).reduce((sum, r) => sum + r.count, 0) / window }));
}
export function platformShare(platforms) {
  const total = platforms.reduce((sum, p) => sum + p.requests, 0);
  return platforms.map(p => ({ ...p, share: total ? p.requests / total : 0 }));
}
export function releaseComposition(releases) {
  return [...releases].sort((a,b) => b.publishedAt.localeCompare(a.publishedAt)).slice(0,8).map(r => ({ ...r, segments: ['windows','macos','linux'].map(id => ({ id, count: r.platforms[id] })) }));
}

export function drawTraffic(target, series) {
  target.replaceChildren();
  if (!series) { target.append(el('p', 'Daily traffic is not available from the source yet.', 'empty')); return; }
  if (!series.length) { target.append(el('p', 'No daily statistics were returned.', 'empty')); return; }
  const total = series.reduce((sum, r) => sum + r.count, 0), peak = Math.max(...series.map(r=>r.count));
  const stats = el('div', undefined, 'chart-summary');
  for (const [label, value] of [['Visits in period',fmt(total)],['Daily average',fmt(total/series.length)],['Daily peak',fmt(peak)]]) { const p=el('p',label); p.append(el('strong',value)); stats.append(p); }
  const width = Math.max(320, target.clientWidth), height = 290, left=50, right=width-16, top=26, bottom=250;
  const magnitude=10**Math.floor(Math.log10(Math.max(1,peak))), limit=Math.max(2,Math.ceil(peak/magnitude)*magnitude);
  const x=i=>left+i/Math.max(1,series.length-1)*(right-left), y=n=>bottom-n/limit*(bottom-top);
  const plot=svg('svg',{viewBox:`0 0 ${width} ${height}`,role:'img','aria-labelledby':'traffic-graph-title traffic-graph-desc',class:'interactive-traffic'});
  plot.append(svg('title',{id:'traffic-graph-title'},'Daily counted page visits'),svg('desc',{id:'traffic-graph-desc'},`${day(series[0].date)} to ${day(series.at(-1).date)}. Total ${total}; peak ${peak}. Orange shows daily counts. Dashed blue shows the trailing seven-day average, starting after seven complete observations. Use the Explore daily visits slider or the daily values table for exact counts.`));
  const defs=svg('defs'); const gradient=svg('linearGradient',{id:'traffic-fill',x1:'0',x2:'0',y1:'0',y2:'1'});
  gradient.append(svg('stop',{offset:'0%','stop-color':'#ff7a45','stop-opacity':'.25'}),svg('stop',{offset:'100%','stop-color':'#ff7a45','stop-opacity':'.015'})); defs.append(gradient); plot.append(defs);
  for (const value of [...new Set([0,Math.ceil(limit/2),limit])]) plot.append(svg('line',{x1:left,x2:right,y1:y(value),y2:y(value),class:'grid-line'}),svg('text',{x:left-12,y:y(value)+4,'text-anchor':'end'},fmt(value)));
  const points=series.map((r,i)=>`${x(i)},${y(r.count)}`).join(' ');
  plot.append(svg('polygon',{points:`${left},${bottom} ${points} ${right},${bottom}`,fill:'url(#traffic-fill)'}),svg('polyline',{points,class:'trend-line'}));
  const average=movingAverage(series).map((r,i)=>r.count===null?null:`${x(i)},${y(r.count)}`).filter(Boolean);
  if(average.length) plot.append(svg('polyline',{points:average.join(' '),class:'average-line'}));
  for(const i of [...new Set([0,Math.floor(series.length/2),series.length-1])]) plot.append(svg('text',{x:x(i),y:height-10,'text-anchor':i===0?'start':i===series.length-1?'end':'middle'},day(series[i].date).replace(/ \d{4}$/,'')));
  const guide=svg('line',{class:'chart-guide',y1:top,y2:bottom}),dot=svg('circle',{class:'chart-dot',r:5}); plot.append(guide,dot);
  const controls=el('div',undefined,'chart-explorer'), label=el('label','Explore daily visits'); label.htmlFor='traffic-day';
  const slider=el('input'); slider.type='range'; slider.id='traffic-day'; slider.min=0; slider.max=series.length-1; slider.step=1;
  const output=el('output'); output.htmlFor='traffic-day';
  const select=i=>{const row=series[i];slider.value=i;slider.setAttribute('aria-valuetext',`${day(row.date)}: ${row.count} counted visits`);output.textContent=`${day(row.date)}  /  ${fmt(row.count)} visits`;guide.setAttribute('x1',x(i));guide.setAttribute('x2',x(i));dot.setAttribute('cx',x(i));dot.setAttribute('cy',y(row.count));};
  slider.addEventListener('input',()=>select(Number(slider.value)));
  plot.addEventListener('pointermove',e=>{const box=plot.getBoundingClientRect();select(Math.max(0,Math.min(series.length-1,Math.round(((e.clientX-box.left)/box.width*width-left)/(right-left)*(series.length-1)))));});
  plot.addEventListener('pointerdown',e=>{const box=plot.getBoundingClientRect();select(Math.max(0,Math.min(series.length-1,Math.round(((e.clientX-box.left)/box.width*width-left)/(right-left)*(series.length-1)))));});
  controls.append(label,output,slider);
  const legend=el('div',undefined,'chart-legend');legend.append(el('span','Daily visits','daily-key'),el('span','7-day average','average-key'));
  target.append(stats,legend,plot,controls);select(series.findIndex(r=>r.count===peak));
}

export function drawPlatforms(target, platforms) {
  target.replaceChildren();
  if(!platforms) {target.append(el('p','GitHub package statistics are unavailable.','empty'));return;}
  const shares=platformShare(platforms),total=platforms.reduce((sum,p)=>sum+p.requests,0);
  if(!total){target.append(el('p','No package requests have been reported.','empty'));return;}
  const graph=svg('svg',{viewBox:'0 0 220 220',role:'img','aria-labelledby':'platform-graph-title',class:'platform-donut'});
  graph.append(svg('title',{id:'platform-graph-title'},`Lifetime package requests: ${shares.map(p=>`${names[p.id]} ${p.requests}`).join(', ')}. Total ${total}.`));
  graph.append(svg('circle',{cx:110,cy:110,r:80,fill:'none',stroke:'#322e35','stroke-width':18}));
  let offset=0;
  for(const p of shares){const segment=svg('circle',{cx:110,cy:110,r:80,fill:'none',stroke:colors[p.id],'stroke-width':18,pathLength:100,'stroke-dasharray':`${p.share*100} ${100-p.share*100}`,'stroke-dashoffset':-offset,transform:'rotate(-90 110 110)'});segment.append(svg('title',{},`${names[p.id]}: ${p.requests} (${fmt(p.share*100)}%)`));graph.append(segment);offset+=p.share*100;}
  graph.append(svg('text',{x:110,y:107,'text-anchor':'middle',class:'donut-total'},fmt(total)),svg('text',{x:110,y:133,'text-anchor':'middle',class:'donut-caption'},'package requests'));
  const legend=el('ul',undefined,'platform-legend');
  for(const p of shares){const li=el('li',undefined,`platform-${p.id}`);li.append(el('span',names[p.id],'platform-name'),el('strong',fmt(p.requests)),el('span',`${fmt(p.share*100)}%`,'platform-percent'));legend.append(li);}
  const wrap=el('div',undefined,'platform-composition');wrap.append(graph,legend);target.append(wrap);
}

export function drawReleases(target, releases) {
  target.replaceChildren();
  if(!releases){target.append(el('p','Release statistics are unavailable.','empty'));return;}
  if(!releases.length){target.append(el('p','No published releases reported.','empty'));return;}
  const rows=releaseComposition(releases),max=Math.max(1,...rows.map(r=>r.requests));
  const list=el('ol',undefined,'release-bars');
  for(const r of rows){const li=el('li');const heading=el('div',undefined,'release-bar-heading');heading.append(el('span',`${r.version}${r.kind==='Prerelease'?' - prerelease':''}`),el('strong',fmt(r.requests)));const bar=svg('svg',{viewBox:'0 0 1000 16',preserveAspectRatio:'none',role:'img','aria-label':`${r.version}: ${r.segments.map(s=>`${names[s.id]} ${s.count}`).join(', ')}. Total ${r.requests}.`});bar.append(svg('rect',{width:1000,height:16,rx:4,fill:'#29262b'}));let start=0;for(const s of r.segments){const rect=svg('rect',{x:start/max*1000,width:s.count/max*1000,height:16,fill:colors[s.id]});rect.append(svg('title',{},`${names[s.id]}: ${s.count} requests`));bar.append(rect);start+=s.count;}li.append(heading,bar);list.append(li);}
  const legend=el('div',undefined,'chart-legend');for(const id of ['windows','macos','linux'])legend.append(el('span',names[id],`platform-${id}`));
  target.append(legend,list,el('p',`Latest ${rows.length} published releases, newest first. Bars use the same scale. Counts are lifetime requests; release ages differ.`,'chart-note'));
}
