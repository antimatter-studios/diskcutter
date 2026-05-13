//! Curated distro catalog — a "popular OS images" picker so users
//! don't have to remember exact download URLs. Each entry is a
//! manual pin to a known-good landing URL; we don't try to scrape
//! the latest version automatically (that's a remote-fetch +
//! signature-verify project of its own — see `url_fetch` and the
//! GPG verify lane on the roadmap).
//!
//! Pure Rust, zero IO. The list is compiled in. Add entries by
//! editing the `entries()` function below; add the `category`,
//! `description`, and `download_url` from the project's official
//! mirrors page. Prefer the project's own `releases` page over
//! third-party mirrors so the URL stays stable.
//!
//! Frontend renders this via the `catalog_list` Tauri command and
//! then hands the chosen URL to the existing `start_download`
//! command (see `url_fetch.rs`).

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    /// Stable identifier — used by the frontend as a React key. Lower
    /// case, hyphenated, never localised.
    pub id: &'static str,
    /// Display name, e.g. "Ubuntu 24.04 LTS".
    pub name: &'static str,
    /// One-line description. Kept short so it fits in the picker.
    pub description: &'static str,
    /// Coarse category for grouping in the UI: "linux", "bsd",
    /// "rescue", "tails-like".
    pub category: &'static str,
    /// Direct ISO/IMG download URL. Pasted into the URL fetch
    /// pipeline on click. The URL must be http(s) — the URL fetch
    /// validator rejects everything else.
    pub download_url: &'static str,
    /// URL where the user can verify the official SHA256SUMS, when
    /// the distro publishes one. Empty string when there is no
    /// canonical hash file (we'd love to wire this up properly with
    /// the GPG verify lane once that lands).
    pub sha256sums_url: &'static str,
    /// Project landing page. Frontend can link this in the entry's
    /// detail row.
    pub homepage: &'static str,
}

/// Return the curated catalog. Hand-maintained list — edit and
/// recompile to refresh. The order here is the order the picker
/// renders, so put the most-requested distros at the top.
pub fn entries() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            id: "ubuntu-24-04-desktop",
            name: "Ubuntu 24.04 LTS Desktop",
            description: "Most popular general-purpose Linux desktop. 5-year support.",
            category: "linux",
            download_url:
                "https://releases.ubuntu.com/24.04/ubuntu-24.04.1-desktop-amd64.iso",
            sha256sums_url: "https://releases.ubuntu.com/24.04/SHA256SUMS",
            homepage: "https://ubuntu.com/download/desktop",
        },
        CatalogEntry {
            id: "ubuntu-24-04-server",
            name: "Ubuntu 24.04 LTS Server",
            description: "Headless server install — no GUI. 5-year support.",
            category: "linux",
            download_url:
                "https://releases.ubuntu.com/24.04/ubuntu-24.04.1-live-server-amd64.iso",
            sha256sums_url: "https://releases.ubuntu.com/24.04/SHA256SUMS",
            homepage: "https://ubuntu.com/download/server",
        },
        CatalogEntry {
            id: "fedora-40-workstation",
            name: "Fedora 40 Workstation",
            description: "Cutting-edge desktop distribution. GNOME by default.",
            category: "linux",
            download_url:
                "https://download.fedoraproject.org/pub/fedora/linux/releases/40/Workstation/x86_64/iso/Fedora-Workstation-Live-x86_64-40-1.14.iso",
            sha256sums_url:
                "https://download.fedoraproject.org/pub/fedora/linux/releases/40/Workstation/x86_64/iso/Fedora-Workstation-40-1.14-x86_64-CHECKSUM",
            homepage: "https://fedoraproject.org/workstation/",
        },
        CatalogEntry {
            id: "debian-12-netinst",
            name: "Debian 12 (Bookworm) netinst",
            description: "Minimal Debian netinstall — fetches packages from a mirror.",
            category: "linux",
            download_url:
                "https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.7.0-amd64-netinst.iso",
            sha256sums_url:
                "https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/SHA256SUMS",
            homepage: "https://www.debian.org/download",
        },
        CatalogEntry {
            id: "linux-mint-22-cinnamon",
            name: "Linux Mint 22 Cinnamon",
            description: "Friendly Ubuntu-based desktop. Cinnamon environment.",
            category: "linux",
            download_url:
                "https://mirrors.edge.kernel.org/linuxmint/stable/22/linuxmint-22-cinnamon-64bit.iso",
            sha256sums_url:
                "https://mirrors.edge.kernel.org/linuxmint/stable/22/sha256sum.txt",
            homepage: "https://linuxmint.com/edition.php?id=313",
        },
        CatalogEntry {
            id: "raspberry-pi-os",
            name: "Raspberry Pi OS (Lite, 64-bit)",
            description: "Headless OS for Raspberry Pi. SD-card image.",
            category: "linux",
            download_url:
                "https://downloads.raspberrypi.com/raspios_lite_arm64_latest",
            sha256sums_url: "",
            homepage: "https://www.raspberrypi.com/software/operating-systems/",
        },
        CatalogEntry {
            id: "tails-6",
            name: "Tails 6",
            description: "Privacy-focused live OS — runs from USB, leaves no trace.",
            category: "tails-like",
            download_url: "https://download.tails.net/tails/stable/tails-amd64-6.10/tails-amd64-6.10.iso",
            sha256sums_url: "https://tails.net/torrents/files/tails-amd64-6.10.iso.sig",
            homepage: "https://tails.net/install/",
        },
        CatalogEntry {
            id: "freebsd-14",
            name: "FreeBSD 14 (memstick)",
            description: "BSD Unix derivative; ports tree, ZFS-first.",
            category: "bsd",
            download_url:
                "https://download.freebsd.org/releases/amd64/amd64/ISO-IMAGES/14.1/FreeBSD-14.1-RELEASE-amd64-memstick.img",
            sha256sums_url:
                "https://download.freebsd.org/releases/amd64/amd64/ISO-IMAGES/14.1/CHECKSUM.SHA256-FreeBSD-14.1-RELEASE-amd64",
            homepage: "https://www.freebsd.org/where/",
        },
        CatalogEntry {
            id: "systemrescue",
            name: "SystemRescue",
            description: "Live rescue toolkit — gparted, testdisk, photorec, ddrescue.",
            category: "rescue",
            download_url:
                "https://fastly-cdn.system-rescue.org/releases/11.02/systemrescue-11.02-amd64.iso",
            sha256sums_url:
                "https://fastly-cdn.system-rescue.org/releases/11.02/sha256sum.txt",
            homepage: "https://www.system-rescue.org/Download/",
        },
    ]
}

/// Tauri command — frontend asks once on mount, caches in React state.
#[tauri::command]
pub fn catalog_list() -> Vec<CatalogEntry> {
    entries()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn entries_returns_a_non_empty_list() {
        let v = entries();
        assert!(!v.is_empty(), "catalog must have at least one entry");
        assert!(
            v.len() >= 5,
            "catalog should have at least 5 entries to be useful, has {}",
            v.len()
        );
    }

    #[test]
    fn ids_are_unique() {
        let v = entries();
        let ids: HashSet<&str> = v.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), v.len(), "duplicate id in catalog");
    }

    #[test]
    fn ids_are_kebab_case() {
        for e in entries() {
            for ch in e.id.chars() {
                assert!(
                    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-',
                    "id {:?} must be lowercase alphanum + hyphen",
                    e.id
                );
            }
        }
    }

    #[test]
    fn every_entry_has_https_download_url() {
        for e in entries() {
            assert!(
                e.download_url.starts_with("https://") || e.download_url.starts_with("http://"),
                "entry {:?} download_url must be http(s)",
                e.id
            );
        }
    }

    #[test]
    fn every_entry_has_https_homepage() {
        for e in entries() {
            assert!(
                e.homepage.starts_with("https://") || e.homepage.starts_with("http://"),
                "entry {:?} homepage must be http(s)",
                e.id
            );
        }
    }

    #[test]
    fn sha256sums_url_when_present_is_https() {
        for e in entries() {
            if e.sha256sums_url.is_empty() {
                continue;
            }
            assert!(
                e.sha256sums_url.starts_with("https://") || e.sha256sums_url.starts_with("http://"),
                "entry {:?} sha256sums_url must be http(s) when present",
                e.id
            );
        }
    }

    #[test]
    fn category_uses_known_value() {
        let known = ["linux", "bsd", "rescue", "tails-like", "windows", "macos"];
        for e in entries() {
            assert!(
                known.contains(&e.category),
                "entry {:?} has unknown category {:?}",
                e.id,
                e.category
            );
        }
    }

    #[test]
    fn name_and_description_are_non_empty() {
        for e in entries() {
            assert!(!e.name.is_empty(), "entry {:?} name empty", e.id);
            assert!(
                !e.description.is_empty(),
                "entry {:?} description empty",
                e.id
            );
            assert!(
                e.description.len() <= 120,
                "entry {:?} description too long ({} chars) — keep tight for the picker",
                e.id,
                e.description.len()
            );
        }
    }

    #[test]
    fn catalog_list_is_a_thin_wrapper_around_entries() {
        // Minor smoke test for the Tauri command itself — guards against
        // refactors that accidentally drop entries on the way through.
        assert_eq!(catalog_list(), entries());
    }
}
