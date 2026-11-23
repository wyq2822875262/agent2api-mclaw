#!/usr/bin/env node
// Pass a directory whose node_modules contains esbuild and qrcode.
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const deps = resolve(process.argv[2] || root);
const require = createRequire(join(deps, 'package.json'));
const { build } = require('esbuild');
const out = join(root, 'ui/mclaw/vendor');
await mkdir(out, { recursive: true });
const result = await build({
  absWorkingDir: root, entryPoints: [require.resolve('qrcode/lib/browser.js')],
  outfile: join(out, 'qrcode.js'), bundle: true, minify: true,
  platform: 'browser', format: 'esm', legalComments: 'eof', metafile: true
});
const notices = new Map();
for (const input of Object.keys(result.metafile.inputs)) {
  let directory = dirname(resolve(root, input));
  while (dirname(directory) !== directory) {
    const metadata = join(directory, 'package.json');
    if (existsSync(metadata)) {
      const pkg = JSON.parse(await readFile(metadata, 'utf8'));
      const identity = pkg.name + '@' + pkg.version;
      if (!notices.has(identity)) {
        const license = ['LICENSE', 'LICENSE.md', 'LICENSE.txt', 'license', 'COPYING'].find(name => existsSync(join(directory, name)));
        if (!license) throw new Error('Missing bundled dependency license: ' + identity);
        const bytes = await readFile(join(directory, license));
        const filename = identity.replaceAll('/', '_').replaceAll('@', '_') + '.LICENSE';
        await writeFile(join(out, filename), bytes);
        notices.set(identity, { package: pkg.name, version: pkg.version, license: filename, sha256: createHash('sha256').update(bytes).digest('hex') });
      }
      break;
    }
    directory = dirname(directory);
  }
}
await writeFile(join(out, 'NOTICE.json'), JSON.stringify(Array.from(notices.values()), null, 2) + '\n');
console.log('Built local QR renderer and included dependency licenses.');
