#!/usr/bin/env node
// i18n key-parity check.
//
// Reads every src/i18n/locales/*.json, recursively extracts dotted key
// paths, then for each locale reports keys present elsewhere but missing
// here (gap) and keys present here but absent everywhere else (orphan —
// usually a typo). Exits non-zero on any drift.
//
// Pure-function shape so tests can import without I/O:
//   extractKeys(obj)             -> Set<string>
//   diffKeys(localesByCode)      -> [{ code, missing[], extra[] }]

import { readFileSync, readdirSync } from 'node:fs';
import { join, basename, extname, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export function extractKeys(obj, prefix = '', out = new Set()) {
  if (obj === null || typeof obj !== 'object' || Array.isArray(obj)) {
    if (prefix) out.add(prefix);
    return out;
  }
  const entries = Object.entries(obj);
  if (entries.length === 0) {
    if (prefix) out.add(prefix);
    return out;
  }
  for (const [k, v] of entries) {
    const next = prefix ? `${prefix}.${k}` : k;
    extractKeys(v, next, out);
  }
  return out;
}

export function diffKeys(localesByCode) {
  const codes = Object.keys(localesByCode).sort();
  const keySets = {};
  const union = new Set();
  for (const code of codes) {
    keySets[code] = extractKeys(localesByCode[code]);
    for (const k of keySets[code]) union.add(k);
  }
  return codes.map((code) => {
    const here = keySets[code];
    const missing = [];
    for (const k of union) if (!here.has(k)) missing.push(k);
    const extra = [];
    for (const k of here) {
      let onlyHere = true;
      for (const other of codes) {
        if (other === code) continue;
        if (keySets[other].has(k)) { onlyHere = false; break; }
      }
      if (onlyHere && codes.length > 1) extra.push(k);
    }
    missing.sort();
    extra.sort();
    return { code, missing, extra };
  });
}

function summariseKeys(keys, max = 8) {
  if (keys.length <= max) return keys.join(', ');
  return `${keys.slice(0, max).join(', ')}, … (+${keys.length - max} more)`;
}

function main() {
  const here = dirname(fileURLToPath(import.meta.url));
  const localesDir = resolve(here, '..', 'src', 'i18n', 'locales');
  const files = readdirSync(localesDir).filter((f) => extname(f) === '.json');
  if (files.length === 0) {
    console.error(`[i18n] no locale json files found in ${localesDir}`);
    process.exit(2);
  }
  const localesByCode = {};
  for (const f of files) {
    const code = basename(f, '.json');
    const raw = readFileSync(join(localesDir, f), 'utf8');
    try {
      localesByCode[code] = JSON.parse(raw);
    } catch (e) {
      console.error(`[i18n] ${f}: invalid JSON — ${e.message}`);
      process.exit(2);
    }
  }
  const reports = diffKeys(localesByCode);
  let drift = false;
  for (const r of reports) {
    if (r.missing.length === 0 && r.extra.length === 0) {
      console.log(`[i18n] ${r.code}.json OK`);
      continue;
    }
    drift = true;
    if (r.missing.length > 0) {
      console.log(`[i18n] ${r.code}.json missing ${r.missing.length} keys: ${summariseKeys(r.missing)}`);
    }
    if (r.extra.length > 0) {
      console.log(`[i18n] ${r.code}.json orphan (only here, possible typo) ${r.extra.length} keys: ${summariseKeys(r.extra)}`);
    }
  }
  if (drift) {
    console.log('[i18n] drift detected — see above');
    process.exit(1);
  }
  console.log('[i18n] all locales in parity');
}

const invokedDirect = (() => {
  if (!process.argv[1]) return false;
  try {
    return resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
})();

if (invokedDirect) main();
