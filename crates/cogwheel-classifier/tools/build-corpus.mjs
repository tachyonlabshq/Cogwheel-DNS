#!/usr/bin/env node
// Build a leakage-safe labelled corpus of ad/tracker domains (label 1) vs benign domains (label 0)
// for training the Cogwheel ad-domain classifier.
//
//   node crates/cogwheel-classifier/tools/build-corpus.mjs --out <dir> [--fetch] [--cache <dir>]
//
// Splits are grouped by registrable domain (eTLD+1) so that no registrable domain appears in more
// than one split. Without that grouping, `ads.example.com` in train and `pixel.example.com` in test
// would leak shared n-grams and inflate every reported metric.
//
// Sources are public and fetched over HTTPS. See docs/architecture/05-classifier.md for the
// licensing notes and the rationale behind the label-hygiene rules.

import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
import crypto from 'node:crypto';
import zlib from 'node:zlib';
import { execFileSync } from 'node:child_process';

const POSITIVE_SOURCES = [
  { name: 'stevenblack', format: 'hosts', url: 'https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts' },
  { name: 'yoyo', format: 'hosts', url: 'https://pgl.yoyo.org/adservers/serverlist.php?hostformat=hosts&showintro=0&mimetype=plaintext' },
  { name: 'adguard-tracking', format: 'adblock', url: 'https://raw.githubusercontent.com/AdguardTeam/AdguardFilters/master/SpywareFilter/sections/tracking_servers.txt' },
  { name: 'hagezi-pro', format: 'adblock', url: 'https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.txt' },
  { name: 'easylist', format: 'adblock', url: 'https://easylist.to/easylist/easylist.txt' },
];

const NEGATIVE_SOURCES = [
  { name: 'tranco', format: 'rankzip', url: 'https://tranco-list.eu/top-1m.csv.zip', column: 1 },
  { name: 'umbrella', format: 'rankzip', url: 'https://s3-us-west-1.amazonaws.com/umbrella-static/top-1m.csv.zip', column: 1 },
  { name: 'majestic', format: 'rankcsv', url: 'https://downloads.majestic.com/majestic_million.csv', column: 2 },
  { name: 'zer0h', format: 'plain', url: 'https://raw.githubusercontent.com/zer0h/top-1000000-domains/master/top-100000-domains' },
];

const PSL_URL = 'https://publicsuffix.org/list/public_suffix_list.dat';

// ---------------------------------------------------------------- args

function parseArgs(argv) {
  const args = { out: null, cache: null, fetch: false, holdout: 0 };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--out') args.out = argv[++i];
    else if (a === '--cache') args.cache = argv[++i];
    else if (a === '--fetch') args.fetch = true;
    else if (a === '--holdout') args.holdout = Number(argv[++i]);
    else if (a === '--help' || a === '-h') { usage(); process.exit(0); }
    else { console.error(`unknown argument: ${a}`); usage(); process.exit(2); }
  }
  if (!args.out) { console.error('--out <dir> is required'); usage(); process.exit(2); }
  if (!args.cache) args.cache = path.join(args.out, 'raw');
  return args;
}

function usage() {
  console.log(`usage: build-corpus.mjs --out <dir> [--fetch] [--cache <dir>] [--holdout N]

  --out <dir>      directory to write train.tsv / val.tsv / test.tsv into
  --cache <dir>    where raw downloads live (default: <out>/raw)
  --fetch          download sources that are missing from the cache
  --holdout N      also emit holdout.tsv with N rows sampled from the test split
`);
}

// ---------------------------------------------------------------- fetch

function download(url, dest) {
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  execFileSync('curl', ['-sSL', '--fail', '--max-time', '600', '-o', dest, url], { stdio: ['ignore', 'ignore', 'inherit'] });
}

function ensureSource(src, cacheDir, doFetch) {
  const ext = src.format === 'rankzip' ? 'zip' : 'txt';
  const dest = path.join(cacheDir, `${src.name}.${ext}`);
  if (fs.existsSync(dest) && fs.statSync(dest).size > 0) return dest;
  if (!doFetch) throw new Error(`missing ${dest} (re-run with --fetch)`);
  process.stderr.write(`fetching ${src.name}... `);
  download(src.url, dest);
  process.stderr.write(`${(fs.statSync(dest).size / 1048576).toFixed(1)} MB\n`);
  return dest;
}

// ---------------------------------------------------------------- public suffix list

function loadPsl(cacheDir, doFetch) {
  const dest = path.join(cacheDir, 'public_suffix_list.dat');
  if (!fs.existsSync(dest) || fs.statSync(dest).size === 0) {
    if (!doFetch) throw new Error(`missing ${dest} (re-run with --fetch)`);
    process.stderr.write('fetching public suffix list... ');
    download(PSL_URL, dest);
    process.stderr.write('ok\n');
  }
  const rules = new Set();
  const wildcards = new Set();
  const exceptions = new Set();
  for (const raw of fs.readFileSync(dest, 'utf8').split('\n')) {
    const line = raw.trim();
    if (!line || line.startsWith('//')) continue;
    if (line.startsWith('!')) exceptions.add(line.slice(1));
    else if (line.startsWith('*.')) wildcards.add(line.slice(2));
    else rules.add(line);
  }
  return { rules, wildcards, exceptions };
}

function makeRegistrable(psl) {
  const cache = new Map();
  return function registrable(host) {
    const hit = cache.get(host);
    if (hit !== undefined) return hit;
    const labels = host.split('.');
    let result = null;
    for (let i = 0; i < labels.length; i++) {
      if (psl.exceptions.has(labels.slice(i).join('.'))) { result = labels.slice(i).join('.'); break; }
    }
    if (result === null) {
      for (let i = 0; i < labels.length; i++) {
        const suffix = labels.slice(i).join('.');
        const parent = labels.slice(i + 1).join('.');
        if (psl.wildcards.has(parent)) { result = i >= 1 ? labels.slice(i - 1).join('.') : null; break; }
        if (psl.rules.has(suffix)) { result = i >= 1 ? labels.slice(i - 1).join('.') : null; break; }
      }
    }
    if (result === null && labels.length >= 2) result = labels.slice(-2).join('.');
    if (cache.size < 4_000_000) cache.set(host, result);
    return result;
  };
}

// ---------------------------------------------------------------- host normalisation

const HOST_RE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/;
const JUNK = new Set([
  'localhost', 'localhost.localdomain', 'local', 'broadcasthost',
  'ip6-localhost', 'ip6-loopback', 'ip6-localnet', 'ip6-mcastprefix',
  'ip6-allnodes', 'ip6-allrouters', 'ip6-allhosts',
]);

function normalizeHost(raw) {
  if (!raw) return null;
  let h = raw.trim().toLowerCase();
  if (!h) return null;
  if (h.endsWith('.')) h = h.slice(0, -1);
  if (h.startsWith('www.')) h = h.slice(4);
  if (!h || h.length > 253) return null;
  if (JUNK.has(h)) return null;
  if (/^\d+\.\d+\.\d+\.\d+$/.test(h)) return null;
  if (!HOST_RE.test(h)) return null;
  const tld = h.slice(h.lastIndexOf('.') + 1);
  if (!/^[a-z]{2,}$/.test(tld)) return null;
  return h;
}

// ---------------------------------------------------------------- parsers

async function eachLine(file, fn) {
  const rl = readline.createInterface({ input: fs.createReadStream(file), crlfDelay: Infinity });
  for await (const line of rl) fn(line);
}

async function parseHosts(file, sink) {
  await eachLine(file, (line) => {
    const s = line.trim();
    if (!s || s.startsWith('#')) return;
    const parts = s.split(/\s+/);
    if (parts.length < 2) return;
    if (!/^(0\.0\.0\.0|127\.0\.0\.1|::1?|::)$/.test(parts[0])) return;
    for (let i = 1; i < parts.length; i++) {
      if (parts[i].startsWith('#')) break;
      const h = normalizeHost(parts[i]);
      if (h) sink.add(h);
    }
  });
}

// Only pure-domain adblock rules (`||domain^`, optionally with $modifiers) become positives.
// URL-path rules and `@@` exception rules are skipped.
async function parseAdblock(file, sink) {
  await eachLine(file, (line) => {
    const s = line.trim();
    if (!s || s.startsWith('!') || s.startsWith('[') || s.startsWith('@@')) return;
    if (!s.startsWith('||')) return;
    const m = /^\|\|([a-z0-9.-]+)\^/.exec(s);
    if (!m) return;
    const rest = s.slice(m[0].length);
    if (rest && !rest.startsWith('$')) return;
    const h = normalizeHost(m[1]);
    if (h) sink.add(h);
  });
}

async function parseRankCsv(file, sink, column) {
  let first = true;
  await eachLine(file, (line) => {
    if (!line) return;
    const cols = line.split(',');
    if (first) { first = false; if (!/^\d+$/.test(cols[0])) return; }
    const h = normalizeHost(cols[column]);
    if (h) sink.add(h);
  });
}

async function parsePlain(file, sink) {
  await eachLine(file, (line) => {
    const h = normalizeHost(line);
    if (h) sink.add(h);
  });
}

function unzipTo(zipPath, destDir) {
  fs.mkdirSync(destDir, { recursive: true });
  execFileSync('unzip', ['-o', '-q', zipPath, '-d', destDir], { stdio: ['ignore', 'ignore', 'inherit'] });
  const entries = fs.readdirSync(destDir).filter((f) => f.endsWith('.csv'));
  if (entries.length === 0) throw new Error(`no csv inside ${zipPath}`);
  return path.join(destDir, entries[0]);
}

// ---------------------------------------------------------------- main

async function main() {
  const args = parseArgs(process.argv);
  fs.mkdirSync(args.out, { recursive: true });
  fs.mkdirSync(args.cache, { recursive: true });

  const psl = loadPsl(args.cache, args.fetch);
  const registrable = makeRegistrable(psl);
  console.error(`psl: ${psl.rules.size} rules, ${psl.wildcards.size} wildcards, ${psl.exceptions.size} exceptions`);

  const positives = new Set();
  for (const src of POSITIVE_SOURCES) {
    const file = ensureSource(src, args.cache, args.fetch);
    const before = positives.size;
    if (src.format === 'hosts') await parseHosts(file, positives);
    else await parseAdblock(file, positives);
    console.error(`  + ${src.name.padEnd(18)} ${String(positives.size - before).padStart(8)} new  (total ${positives.size})`);
  }

  const negatives = new Set();
  for (const src of NEGATIVE_SOURCES) {
    const file = ensureSource(src, args.cache, args.fetch);
    const before = negatives.size;
    if (src.format === 'rankzip') {
      const csv = unzipTo(file, path.join(args.cache, `${src.name}-unpacked`));
      await parseRankCsv(csv, negatives, src.column);
    } else if (src.format === 'rankcsv') {
      await parseRankCsv(file, negatives, src.column);
    } else {
      await parsePlain(file, negatives);
    }
    console.error(`  + ${src.name.padEnd(18)} ${String(negatives.size - before).padStart(8)} new  (total ${negatives.size})`);
  }

  // Label hygiene. A host on any blocklist is a positive, so remove it from the negatives.
  let exactOverlap = 0;
  for (const h of positives) if (negatives.delete(h)) exactOverlap++;

  // A negative whose registrable domain is itself blocklisted (e.g. any *.doubleclick.net) is not a
  // trustworthy negative — drop it rather than teach the model that doubleclick.net is benign.
  let registrableOverlap = 0;
  for (const h of [...negatives]) {
    const reg = registrable(h);
    if (reg && positives.has(reg)) { negatives.delete(h); registrableOverlap++; }
  }
  console.error(`hygiene: removed ${exactOverlap} exact and ${registrableOverlap} registrable-domain overlaps from negatives`);
  console.error(`corpus: ${positives.size} positives, ${negatives.size} negatives`);

  // Leakage-safe split: bucket by registrable domain, 80/10/10.
  const splitOf = (reg) => {
    const b = crypto.createHash('sha256').update(reg).digest().readUInt32BE(0) % 100;
    return b < 80 ? 'train' : b < 90 ? 'val' : 'test';
  };

  const rows = { train: [], val: [], test: [] };
  let skipped = 0;
  const push = (host, label) => {
    const reg = registrable(host);
    if (!reg) { skipped++; return; }
    rows[splitOf(reg)].push(`${host}\t${label}`);
  };
  for (const h of positives) push(h, 1);
  for (const h of negatives) push(h, 0);
  if (skipped) console.error(`skipped ${skipped} hosts with no registrable domain`);

  // Deterministic shuffle (xorshift32) so SGD sees interleaved labels without needing a shuffle pass.
  const shuffle = (arr, seed) => {
    let s = seed >>> 0;
    const rnd = () => { s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return s / 4294967296; };
    for (let i = arr.length - 1; i > 0; i--) {
      const j = Math.floor(rnd() * (i + 1));
      [arr[i], arr[j]] = [arr[j], arr[i]];
    }
  };

  for (const [name, arr] of Object.entries(rows)) {
    shuffle(arr, 0x5eed1234);
    const pos = arr.reduce((a, r) => a + (r.charCodeAt(r.length - 1) === 49 ? 1 : 0), 0);
    fs.writeFileSync(path.join(args.out, `${name}.tsv`), arr.join('\n') + '\n');
    console.error(`${name.padEnd(6)} ${String(arr.length).padStart(9)} rows  ${String(pos).padStart(8)} pos  ${(100 * pos / arr.length).toFixed(1)}% positive`);
  }

  // A small, committed holdout keeps the model-quality regression test cheap to run in CI.
  if (args.holdout > 0) {
    const sample = rows.test.slice(0, args.holdout);
    const file = path.join(args.out, 'holdout.tsv');
    fs.writeFileSync(file, sample.join('\n') + '\n');
    fs.writeFileSync(`${file}.gz`, zlib.gzipSync(fs.readFileSync(file), { level: 9 }));
    console.error(`holdout ${sample.length} rows -> ${file}(.gz)`);
  }

  // Assert the split really is leakage-free; a silent regression here invalidates every metric.
  const seen = new Map();
  let leaks = 0;
  for (const [name, arr] of Object.entries(rows)) {
    for (const r of arr) {
      const reg = registrable(r.slice(0, r.length - 2));
      const prev = seen.get(reg);
      if (prev === undefined) seen.set(reg, name);
      else if (prev !== name) leaks++;
    }
  }
  if (leaks > 0) {
    console.error(`LEAKAGE CHECK FAILED: ${leaks} registrable domains span multiple splits`);
    process.exit(1);
  }
  console.error('leakage check: PASS (no registrable domain spans splits)');
}

main().catch((err) => { console.error(err); process.exit(1); });
