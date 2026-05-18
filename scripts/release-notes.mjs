#!/usr/bin/env node
// release-notes.mjs <version>
//
// Print the CHANGELOG.md section for <version> on stdout. Used by the
// release workflow as the GitHub release body so the published notes
// match the curated changelog entries instead of a generic placeholder.
//
// Match rule: case-insensitive `## [<version>]` heading, exact version
// string. Extracts everything from the line AFTER the heading up to
// (but not including) the next `## ` heading or EOF. Trims surrounding
// blank lines.
//
// Exit codes:
//   0 — section found, body written to stdout
//   1 — wrong usage
//   2 — section not found (treat as a workflow failure: the release
//        notes are derivable from the changelog by contract, so a
//        missing section means someone forgot to update it)

import fs from 'node:fs';
import path from 'node:path';
import url from 'node:url';

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, '..');
const CHANGELOG = path.join(REPO_ROOT, 'CHANGELOG.md');

const version = process.argv[2];
if (!version) {
  console.error('usage: release-notes.mjs <version>');
  process.exit(1);
}

const escaped = version.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const headingRe = new RegExp(`^##\\s*\\[${escaped}\\]`, 'i');

const lines = fs.readFileSync(CHANGELOG, 'utf8').split('\n');

let start = -1;
for (let i = 0; i < lines.length; i++) {
  if (headingRe.test(lines[i])) {
    start = i;
    break;
  }
}
if (start < 0) {
  console.error(`release-notes: no "## [${version}]" section in ${CHANGELOG}`);
  process.exit(2);
}

let end = lines.length;
for (let i = start + 1; i < lines.length; i++) {
  // Any subsequent level-2 heading ends the section. Bottom-of-file
  // link references (e.g. `[2026.5.18]: https://…`) are not headings
  // so they're left out naturally.
  if (/^## /.test(lines[i])) {
    end = i;
    break;
  }
}

const body = lines.slice(start + 1, end).join('\n').replace(/^\s+|\s+$/g, '');
process.stdout.write(body + '\n');
