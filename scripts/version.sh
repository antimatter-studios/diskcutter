#!/usr/bin/env bash
#
# Shared version utilities for dev builds and releases.
# Source this file; do not execute directly.
#
# Version format: YYYY.M.D-N
#   YYYY.M.D  — CalVer date (no leading zeros; Tauri enforces strict semver)
#   -0        — stable release suffix
#   -1, -2 …  — dev build suffix (auto-increments per day)

# Emit YYYY.M.D from the current system clock.
calver_today() {
  node -p "const d=new Date(); \`\${d.getFullYear()}.\${d.getMonth()+1}.\${d.getDate()}\`"
}

# Emit the stable release version for today: YYYY.M.D-0
calver_release() {
  echo "$(calver_today)-0"
}

# Given the path to a dev-updates/latest.json, emit the next dev version
# for today (YYYY.M.D-N), auto-incrementing N within the same day.
calver_next_dev() {
  local manifest="${1:-}"
  local base
  base="$(calver_today)"
  local counter=1
  if [[ -f "$manifest" ]]; then
    local existing
    existing="$(node -p "try{require('$manifest').version}catch(e){''}" 2>/dev/null || echo "")"
    if [[ "$existing" == "${base}-"* ]]; then
      local prev="${existing##*-}"
      [[ "$prev" =~ ^[0-9]+$ ]] && counter=$(( prev + 1 ))
    fi
  fi
  echo "${base}-${counter}"
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
