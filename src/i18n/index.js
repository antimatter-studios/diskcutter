// i18n/index.js — i18next bootstrap with auto-discovery of locale catalogs.
//
// Drop a new `./locales/<code>.json` and it becomes available automatically
// (Vite's import.meta.glob bundles them at build time). Each catalog must
// expose `language.name` so the picker can render its own native label.

import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';

// Eagerly import every locale JSON. Filenames like `./locales/de.json` become
// `de` after stripping the `./locales/` prefix and `.json` suffix.
const modules = import.meta.glob('./locales/*.json', { eager: true });

const resources = {};
const available = [];

for (const [path, mod] of Object.entries(modules)) {
  const match = path.match(/\.\/locales\/([^/]+)\.json$/);
  if (!match) continue;
  const code = match[1];
  const data = mod.default || mod;
  resources[code] = { translation: data };
  available.push({
    code,
    name: (data && data.language && data.language.name) || code,
  });
}

available.sort((a, b) => a.name.localeCompare(b.name));

const STORAGE_KEY = 'diskcutter.language';

function pickInitialLanguage() {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved && resources[saved]) return saved;
  } catch {
    // localStorage may throw in restrictive contexts; fall through.
  }
  if (typeof navigator !== 'undefined' && navigator.language) {
    const primary = navigator.language.toLowerCase();
    if (resources[primary]) return primary;
    const short = primary.split('-')[0];
    if (resources[short]) return short;
  }
  if (resources.en) return 'en';
  return available[0]?.code || 'en';
}

i18n
  .use(initReactI18next)
  .init({
    resources,
    lng: pickInitialLanguage(),
    fallbackLng: 'en',
    interpolation: { escapeValue: false },
    returnEmptyString: false,
  });

// Hydrate from the SQLite-backed config, then install the persistence
// listener. Doing it in this order avoids the navigator-detected fallback
// clobbering the stored preference during init. localStorage stays in
// the loop as a warm cache so first paint doesn't flash a different language.
invoke('config_get', { key: 'language' })
  .then(async (saved) => {
    if (saved && resources[saved] && saved !== i18n.language) {
      await i18n.changeLanguage(saved);
    }
  })
  .catch(() => {})
  .finally(() => {
    i18n.on('languageChanged', (lng) => {
      try { localStorage.setItem(STORAGE_KEY, lng); } catch {
        // ignore — warm cache only
      }
      invoke('config_set', { key: 'language', value: lng }).catch(() => {});
    });
  });

export { available as availableLanguages };
export default i18n;
