#!/usr/bin/env bash
#
# Shared version utilities for dev builds and releases.
# Source this file; do not execute directly.
#
# Version format: YYYY.M.D-N[-branch-slug]
#   YYYY.M.D      — CalVer date (no leading zeros; Tauri enforces strict semver)
#   -0            — stable release suffix
#   -1, -2 …      — dev build suffix (auto-increments per day, global across branches)
#   -branch-slug  — present when built from a non-main branch; stripped of common
#                   prefixes (feature/, fix/, chore/, etc.) and normalized to
#                   lowercase alphanumeric + hyphens, max 24 chars

# Emit YYYY.M.D from the current system clock.
calver_today() {
  node -p "const d=new Date(); \`\${d.getFullYear()}.\${d.getMonth()+1}.\${d.getDate()}\`"
}

# Emit the stable release version for today: YYYY.M.D-0
calver_release() {
  echo "$(calver_today)-0"
}

# Given the path to a dev-updates/latest.json, emit the next dev version
# for today (YYYY.M.D-N[-branch-slug]), auto-incrementing N globally across
# all branches so no two branches collide on the same N for a given day.
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
          .map(v => { const m = v.slice(base.length + 1).match(/^(\d+)/); return m ? parseInt(m[1]) : 0; })
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
      prev="${prev%%-*}"
      [[ "$prev" =~ ^[0-9]+$ ]] && counter=$(( prev + 1 ))
    fi
  fi

  echo "${base}-${counter}${branch_slug}"
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
