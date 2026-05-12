# macOS code signing & notarization

How to safely set up macOS code signing for Disk Cutter's release pipeline so the
built `.dmg` / `.app` runs on any Mac without Gatekeeper warnings.

## Two-step process: **sign** + **notarize**

Signing alone isn't enough on modern macOS. Apps downloaded from the internet get
a quarantine bit; Gatekeeper rejects anything that isn't *also* notarized by Apple.

| State | What user sees |
|---|---|
| Unsigned | "App is damaged, move to Trash" |
| Signed only | "Unidentified developer" warning, right-click → Open to bypass |
| Signed + notarized + stapled | Opens cleanly on any Mac, no warnings |

## What you need

1. **Apple Developer Program** membership ($99/yr) — required for the cert.
2. **Developer ID Application** certificate (not "Mac App Store" — different cert,
   MAS distribution only).
   - Xcode → Settings → Accounts → Manage Certificates → `+` → Developer ID
     Application
   - Export from Keychain as `.p12` with a strong password.
3. **App Store Connect API key** for `notarytool` (safer than Apple ID +
   app-specific password — scoped, revocable, no 2FA breakage):
   - appstoreconnect.apple.com → Users and Access → Integrations → Keys → `+`
   - Role: **Developer** (sufficient for notarization)
   - Download the `.p8` file once (cannot re-download), note the **Key ID** and
     **Issuer ID**.

## GitHub secrets (safe storage)

Base64-encode the binary files before pasting into secrets:

```sh
base64 -i Certificates.p12 | pbcopy        # → APPLE_CERTIFICATE
base64 -i AuthKey_XXXXXX.p8 | pbcopy       # → APPLE_API_KEY
```

Repo → Settings → Secrets and variables → Actions → New secret:

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | base64 of `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Your Name (TEAMID)` — copy exact string from Keychain |
| `APPLE_TEAM_ID` | 10-char team ID (Apple Developer → Membership) |
| `APPLE_API_ISSUER` | Issuer ID UUID from App Store Connect |
| `APPLE_API_KEY_ID` | 10-char Key ID |
| `APPLE_API_KEY` | base64 of `.p8` |
| `KEYCHAIN_PASSWORD` | any random string (temp keychain on runner) |

## Wire into the release workflow

In [.github/workflows/release.yml](../.github/workflows/release.yml), extend the
`tauri-action` step with an `env:` block carrying the signing/notarization
secrets. The action only consumes the Apple vars when it runs on a macOS runner;
they're harmless on the Linux/Windows matrix legs.

```yaml
      - name: Materialize App Store Connect API key (macOS only)
        if: matrix.platform == 'macos-latest'
        run: |
          mkdir -p "$RUNNER_TEMP"
          echo "$APPLE_API_KEY_B64" | base64 -d > "$RUNNER_TEMP/AuthKey.p8"
        env:
          APPLE_API_KEY_B64: ${{ secrets.APPLE_API_KEY }}

      - name: Build & upload (tauri-action)
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          # macOS signing + notarization (ignored on non-macOS runners)
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
          APPLE_API_ISSUER: ${{ secrets.APPLE_API_ISSUER }}
          APPLE_API_KEY: ${{ secrets.APPLE_API_KEY_ID }}
          APPLE_API_KEY_PATH: ${{ runner.temp }}/AuthKey.p8
          KEYCHAIN_PASSWORD: ${{ secrets.KEYCHAIN_PASSWORD }}
        with:
          tagName: ${{ startsWith(github.ref, 'refs/tags/') && github.ref_name || '' }}
          releaseName: ${{ startsWith(github.ref, 'refs/tags/') && format('Disk Cutter {0}', github.ref_name) || '' }}
          releaseDraft: true
          prerelease: false
          args: ${{ matrix.args }}
```

`tauri-action` will:

1. Import the `.p12` into a temporary keychain on the runner.
2. Codesign the `.app` with hardened runtime (`--options runtime`, on by default).
3. Submit the bundle to Apple's notary service via `notarytool` using the API key.
4. Staple the notarization ticket into the `.app` / `.dmg`.
5. Tear down the temporary keychain.

## Safety best practices

- **Use the API key, not Apple ID + app password** — app passwords give broad
  account access; API keys are scoped to notarization and revocable in one click.
- **Never log secrets** — don't `echo "$APPLE_CERTIFICATE"`. GitHub masks the
  raw secret, but `base64 -d` output is NOT masked.
- **Restrict signing to tag pushes** — never sign on PR builds. A malicious PR
  could otherwise exfiltrate the secrets. GitHub blocks secrets from
  forked-PR runs by default; keep it that way (don't enable "Send secrets to
  workflows from forks").
- **Pin action SHAs** if paranoid: `uses: tauri-apps/tauri-action@<sha>` instead
  of `@v0` — prevents a compromised release tag from running with your signing
  creds.
- **Rotate yearly** — `.p12` cert and `.p8` key. Revoke immediately if a runner
  or repo collaborator is compromised.
- **Entitlements** — for an app that writes raw disks you may want
  `com.apple.security.device.usb` and/or a hardened-runtime exception (e.g.
  `com.apple.security.cs.allow-unsigned-executable-memory` — avoid unless
  needed). Edit `src-tauri/Entitlements.plist` and reference it in
  `tauri.conf.json` under `bundle.macOS.entitlements`.

## Will it run on any Mac?

**With signing + notarization + stapling: yes.** Any macOS 10.15+ machine, no
warnings, no internet required at launch (stapling embeds the notarization
ticket in the bundle).

Caveats:

- **Architecture** — the release matrix builds `universal-apple-darwin`, so both
  Apple Silicon and Intel Macs work from one artifact.
- **macOS < 10.15** is rare now and won't notarize-check; the app still runs.
- **Raw disk writes need admin/root** — Disk Cutter will need to prompt for the
  user's password (`authopen`, `SMJobBless`, or a privileged helper tool).
  Signing/notarization does NOT grant elevated privileges; this is an
  app-design concern, not a pipeline one.

## Verifying locally

After downloading the released `.dmg`:

```sh
# Strip nothing — simulate a real user download
xattr -w com.apple.quarantine "0083;0;Safari;" Disk\ Cutter.app

# Should report "accepted" with "source=Notarized Developer ID"
spctl -a -vvv -t install Disk\ Cutter.app

# Should report "Notarization checked" and list the ticket
codesign --verify --deep --strict --verbose=4 Disk\ Cutter.app
stapler validate Disk\ Cutter.app
```

If `spctl` rejects the bundle, notarization didn't complete or wasn't stapled.
Check the Action logs for the `notarytool submit` output.
