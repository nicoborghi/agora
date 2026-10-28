// Builds Agora's emoji font (ui/fonts/noto-emoji.woff2): Noto colour with only the yellow version
// of each emoji (skin tones taken out), compressed as woff2.
//
// Input: 2D/fonts/Noto-COLRv1-noflags.ttf from https://github.com/googlefonts/noto-emoji.
// Tools, each in a folder of its own and used only here:
//   - hb-subset from the HarfBuzz release for your system (https://github.com/harfbuzz/harfbuzz/releases);
//     the WebAssembly build in npm packages drops the colour tables.
//   - `npm install wawoff2` for the woff2 compression.
//
// Usage: node make-emoji-font.mjs <hb-subset> <wawoff2-dir> <Noto-COLRv1-noflags.ttf> ui/fonts/noto-emoji.woff2
import { execFileSync } from 'child_process';
import { createRequire } from 'module';
import fs from 'fs';
import os from 'os';
import path from 'path';

const [hbSubset, toolDir, input, output] = process.argv.slice(2);
const wawoff2 = createRequire(`${toolDir}/package.json`)('wawoff2');

const subset = path.join(os.tmpdir(), 'agora-emoji-subset.ttf');
// Everything the font has, minus the five skin-tone modifiers (and with them every toned sequence).
execFileSync(hbSubset, [input, '--unicodes=*', '--unicodes-=1F3FB-1F3FF', '--layout-features=*', `--output-file=${subset}`]);
const out = await wawoff2.compress(fs.readFileSync(subset));
fs.writeFileSync(output, out);
fs.unlinkSync(subset);
console.log(`${(out.length / 1e6).toFixed(2)} MB`);
