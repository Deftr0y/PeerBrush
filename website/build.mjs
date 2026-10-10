import { cp, mkdir } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(fileURLToPath(import.meta.url));
const output = join(root, 'dist');
await mkdir(output, { recursive: true });
for (const file of ['index.html', 'styles.css', 'main.js', 'platform.js', 'showcase.js', 'community.html', 'releases.html', 'releases.js', 'release-page.js']) {
  await cp(join(root, file), join(output, file));
}
await cp(join(root, 'public'), output, { recursive: true });
console.log('Built PeerBrush website in dist/');
