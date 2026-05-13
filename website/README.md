# Disk Cutter — Site

GitHub Pages site for Disk Cutter. Plain static HTML/CSS, no build step.

## Files

- `index.html` — single page; edit text directly
- `styles.css` — brutalist stylesheet, matches the app
- `icon.svg` — the blade mark (same source as the Tauri app icon)

## Deploy to GitHub Pages

1. Push this folder (or its contents) to a repo.
2. **Option A** — put files at the repo root and enable Pages on the `main` branch.
3. **Option B** — keep them under `site/` and configure Pages to serve from `/site` (Settings → Pages → Source).

Pages will serve `index.html` as the homepage. No Jekyll, no Hugo, no config needed.

## Editing

Everything is plain HTML — open `index.html` in your editor and change copy directly. Sections are commented:

- Hero
- Warning strip
- Features (6 cards — duplicate or delete `.feature` blocks)
- Screenshot (drop a `screenshot.png` in this folder and uncomment the `<img>` tag in the screenshot section)
- Download (per-OS cards — wire up real release URLs)
- About / FAQ
- Footer

## Replacing the screenshot

```html
<div class="screenshot-frame">
  <img src="screenshot.png" alt="Disk Cutter writing two disks in parallel">
</div>
```

Use a wide screenshot at 1920×1200 or similar 16:10 ratio for best fit.

## Replacing the icon

The site loads `icon.svg` for the brand mark, the hero, and the favicon. Drop in a new SVG with the same name to replace all three.

## Adding pages

Add more HTML files in this folder. The top bar nav lives in `<header class="topbar">` — link to them there.

## Local preview

Open `index.html` in a browser, or run any static server:

```bash
python3 -m http.server 8000 --directory site
# then visit http://localhost:8000
```
