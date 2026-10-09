export function detectPlatform({platform='',userAgent='',maxTouchPoints=0}={}) {
  const text=`${platform} ${userAgent}`.toLowerCase();
  if(/android|iphone|ipad|ipod|cros/.test(text) || (/mac/.test(text)&&maxTouchPoints>1)) return null;
  if(/windows|win32|win64/.test(text))return 'windows';
  if(/mac/.test(text))return 'macos';
  if(/linux|x11/.test(text))return 'linux';
  return null;
}
export function completeRelease(release) {
  if(!release || release.draft || !Array.isArray(release.assets))return null;
  const definitions=[{id:'windows',name:'Windows',architecture:'x64',file:'PeerBrush-Windows.zip'},{id:'linux',name:'Linux',architecture:'x64',file:'PeerBrush-Linux.zip'},{id:'macos',name:'macOS',architecture:'arm64',file:'PeerBrush-macOS.zip'}];
  const platforms=definitions.map(item=>{const asset=release.assets.find(a=>a.name===item.file);return asset?{...item,url:asset.browser_download_url,bytes:asset.size}:null;});
  if(platforms.some(p=>!p || !safeDownloadUrl(p.url)))return null;
  return {version:release.tag_name,label:release.prerelease?'Development build':release.tag_name,releaseUrl:release.html_url,platforms};
}
export function safeDownloadUrl(value) {
  try {const url=new URL(value);return url.protocol==='https:'&&url.hostname==='github.com'&&!url.username&&!url.password&&url.pathname.startsWith('/Deftr0y/PeerBrush/releases/download/');}catch{return false;}
}
