#!/usr/bin/env bash
#
# Shared version utilities for dev builds and releases.
# Source this file; do not execute directly.
#
# Version format: YYYY.M.D-<suffix>
#   YYYY.M.D      — CalVer date (no leading zeros; Tauri enforces strict semver)
#   -0, -1, -2 …  — stable release; the number increments for a second (hotfix)
#                   release on the same date
#   -dev.N        — dev build (auto-increments per day, global across branches)
#   -branch-slug  — appended to a dev version when built from a non-main branch;
#                   stripped of common prefixes (feature/, fix/, chore/, etc.)
#                   and normalized to lowercase alphanumeric + hyphens, max 24
#
# The `dev.` prefix exists so a version string identifies its channel. Without
# it, stable `-1` and dev `-1` on the same date are the same string describing
# two different artifacts.
#
# NOTE ON ORDERING: raw semver gets this wrong, deliberately so, and we override
# it rather than bend the scheme around it. Semver ranks numeric prerelease
# identifiers BELOW alphanumeric ones, so it considers 2026.7.29-1 (stable) to
# be older than 2026.7.29-dev.1. Left alone, a machine on a dev build that
# switched to the stable channel would be told it was already up to date.
#
# tauri-plugin-updater exposes `UpdaterBuilder::version_comparator`, which
# replaces the `remote > current` test outright. src-tauri/src/updater.rs
# installs one that parses this scheme — date first, then channel (stable beats
# dev on the same date), then the counter. That is the single source of truth
# for "is this an update"; semver ordering of these strings is not relied on
# anywhere. See the tests in that module.

# Emit YYYY.M.D from the current system clock.
calver_today() {
  node -p "const d=new Date(); \`\${d.getFullYear()}.\${d.getMonth()+1}.\${d.getDate()}\`"
}

# Emit the first stable release version for today: YYYY.M.D-0
#
# Only the fallback for a tagless workflow_dispatch build. A tag push uses the
# tag verbatim, so a same-day hotfix is tagged YYYY.M.D-1 by hand rather than
# guessed at here.
calver_release() {
  echo "$(calver_today)-0"
}

# Given the path to a dev-updates/latest.json, emit the next dev version
# for today (YYYY.M.D-dev.N[-branch-slug]), auto-incrementing N globally across
# all branches so no two branches collide on the same N for a given day.
#
# Reads legacy `YYYY.M.D-N` entries as well as `YYYY.M.D-dev.N`, so a manifest
# written before the channel prefix existed still advances the counter instead
# of restarting at 1 and colliding with a build that already shipped.
calver_next_dev() {
  local manifest="${1:-}"
  local base
  base="$(calver_today)"

  # Derive branch slug from the current git branch.
  local branch_slug=""
  local branch
  branch="$(git branch --show-current 2>/dev/null || echo "")"
  # Strip the first path component (feature/, fix/, chore/, etc.)
  [[ "$branch" == */* ]] && branch="${branch#*/}"
  # Normalize: lowercase, squeeze non-alnum runs into single hyphen, trim ends
  branch="$(printf '%s' "$branch" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9' '-' | sed 's/^-*//' | sed 's/-*$//')"
  branch="${branch:0:24}"
  if [[ -n "$branch" && "$branch" != "main" && "$branch" != "master" && "$branch" != "develop" ]]; then
    branch_slug="-${branch}"
  fi

  # Find the highest N used today across ALL branches in updates.json so that
  # concurrent worktrees don't emit duplicate N values.
  local counter=1
  local updates_json
  updates_json="$(dirname "${manifest:-/nonexistent}")/updates.json"
  if [[ -f "$updates_json" ]]; then
    local max_n
    max_n="$(node -p "
      try {
        const u = JSON.parse(require('fs').readFileSync('$updates_json', 'utf8'));
        const base = '$base';
        const nums = (u.dev || [])
          .map(e => e.version)
          .filter(v => v.startsWith(base + '-'))
          .map(v => { const m = v.slice(base.length + 1).match(/^(?:dev\.)?(\d+)/); return m ? parseInt(m[1]) : 0; })
          .filter(n => n > 0);
        nums.length ? Math.max(...nums) : 0;
      } catch(e) { 0 }
    " 2>/dev/null || echo "0")"
    [[ "$max_n" =~ ^[0-9]+$ ]] && counter=$(( max_n + 1 ))
  elif [[ -n "$manifest" && -f "$manifest" ]]; then
    # Fallback: no updates.json yet, read latest.json
    local existing
    existing="$(node -p "try{JSON.parse(require('fs').readFileSync('$manifest','utf8')).version}catch(e){''}" 2>/dev/null || echo "")"
    if [[ "$existing" == "${base}-"* ]]; then
      local prev="${existing#${base}-}"
      prev="${prev#dev.}"     # tolerate both dev.N and the legacy bare N
      prev="${prev%%-*}"
      [[ "$prev" =~ ^[0-9]+$ ]] && counter=$(( prev + 1 ))
    fi
  fi

  echo "${base}-dev.${counter}${branch_slug}"
}

# Patch tauri.conf.json, package.json, and Cargo.toml to the given version.
# Usage: version_bump "2026.5.29-0"
version_bump() {
  local ver="$1"
  local root
  root="$(git rev-parse --show-toplevel)"

  node -e "
    const fs = require('fs');
    for (const p of ['$root/src-tauri/tauri.conf.json', '$root/package.json']) {
      const c = JSON.parse(fs.readFileSync(p, 'utf8'));
      c.version = '$ver';
      fs.writeFileSync(p, JSON.stringify(c, null, 2) + '\n');
    }
  "

  # Cargo.toml: replace the first `version = "..."` line in [package]
  sed -i.bak "s/^version = \"[^\"]*\"/version = \"$ver\"/" "$root/src-tauri/Cargo.toml"
  rm -f "$root/src-tauri/Cargo.toml.bak"

  echo "→ bumped version to $ver in tauri.conf.json, package.json, Cargo.toml"
}
