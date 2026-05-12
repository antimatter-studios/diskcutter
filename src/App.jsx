import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open } from '@tauri-apps/plugin-dialog';
import {
  WindowChrome, Sidebar, DangerBanner, Toolbar,
  JobRow, DiskPickerSheet, PrefsView, PREFS_DEFAULTS,
} from './components.jsx';
import i18n from './i18n/index.js';
import {
  useTweaks, TweaksPanel, TweakSection,
  TweakRadio, TweakToggle, TweakColor,
} from './tweaks-panel.jsx';
import {
  formatBytes, formatBps, formatDuration, formatSession, makeJob,
} from './format.js';
import {
  applyJobUpdate, applyJobComplete, applyJobFailure,
} from './job-reducers.js';
import {
  computeScene, sceneToTitleKey, computeSessionStats, planStart,
} from './app-derive.js';

// Theme palette is sourced from CSS :root vars (--accent-1..4), so theme switching is just a CSS swap.
function readThemeAccent(n, fallback) {
  if (typeof window === 'undefined') return fallback;
  const v = getComputedStyle(document.documentElement).getPropertyValue(`--accent-${n}`).trim();
  return v || fallback;
}
const ACCENT_OPTIONS = [
  readThemeAccent(1, '#ff4a17'),
  readThemeAccent(2, '#1f6feb'),
  readThemeAccent(3, '#7a5af8'),
  readThemeAccent(4, '#0c0c0b'),
];

const TWEAK_DEFAULTS = {
  platform: 'mac',
  decorations: 'custom',
  accent: ACCENT_OPTIONS[0],
  verboseTitle: true,
};

// Known error codes carry localized title+detail; unknown ones fall back to
// the generic error label plus whatever message the backend supplied.
const ERROR_CODES = ['ETOOBIG', 'EHASHMISMATCH', 'EUNSUPPORTED', 'EIMAGE', 'ETARGET', 'EIO', 'ECANCELLED', 'ESIZEMISMATCH', 'ENEEDS_FDA'];

function App() {
  // `tw` for tweak state — `t` is reserved for the i18n translator below.
  const [tw, setTweak] = useTweaks(TWEAK_DEFAULTS);
  const { t } = useTranslation();
  const [jobs, setJobs] = useState([]);
  const [disks, setDisks] = useState([]);
  const [confirmed, setConfirmed] = useState(false);
  const [pickerJob, setPickerJob] = useState(null);
  const [expanded, setExpanded] = useState({});
  const [activeNav, setActiveNav] = useState('queue');
  const [buildInfo, setBuildInfo] = useState('—');
  const [orphanPids, setOrphanPids] = useState([]);
  // Prefs are mirrored from the SQLite-backed config_all() at mount. Empty
  // string from the DB means "never set" → use the default from
  // PREFS_DEFAULTS so the UI never renders an empty <select>.
  const [prefs, setPrefs] = useState(() => ({ ...PREFS_DEFAULTS }));
  const sessionStartRef = useRef(Date.now());
  const [, forceTick] = useState(0);

  useEffect(() => {
    const i = setInterval(() => forceTick((n) => n + 1), 10000);
    return () => clearInterval(i);
  }, []);

  useEffect(() => {
    invoke('list_disks').then(setDisks).catch((e) => console.error('list_disks failed', e));
    invoke('app_info').then((info) => {
      const osShort = { macos: 'darwin', windows: 'win32', linux: 'linux' }[info.os] || info.os;
      const archShort = { aarch64: 'arm64', x86_64: 'x64' }[info.arch] || info.arch;
      setBuildInfo(`${info.version} · ${osShort}/${archShort}${info.is_privileged ? ' · root' : ''}`);
    }).catch((e) => console.error('app_info failed', e));
    invoke('find_orphan_helpers').then(setOrphanPids).catch((e) => console.error('find_orphan_helpers failed', e));
    // Hydrate every pref in one round-trip. Missing/empty keys fall through
    // to PREFS_DEFAULTS so the UI shows a sensible value until the user
    // touches the control for the first time.
    invoke('config_all').then((all) => {
      setPrefs((prev) => {
        const next = { ...prev };
        for (const k of Object.keys(PREFS_DEFAULTS)) {
          const v = all && all[k];
          next[k] = (v != null && v !== '') ? String(v) : PREFS_DEFAULTS[k];
        }
        // language default: whatever i18n decided at boot
        if (!next.language) next.language = i18n.language;
        return next;
      });
    }).catch((e) => console.error('config_all failed', e));
  }, []);

  // Persist + apply a single pref change. Side effects (language, theme) are
  // handled inline so the UI updates in lockstep with the DB write.
  const setPref = useCallback((key, value) => {
    const v = String(value);
    setPrefs((prev) => ({ ...prev, [key]: v }));
    invoke('config_set', { key, value: v }).catch((e) => console.error('config_set failed', key, e));
    if (key === 'language') {
      i18n.changeLanguage(v).catch(() => {});
    }
    if (key === 'auto.eject' && v === 'true') {
      // TODO: invoke('eject_disk', { device }) once the backend ships it.
      console.warn('auto.eject enabled, but eject_disk command is not implemented yet');
    }
  }, []);

  // Theme: data-theme attribute on <html> drives the dark palette swap.
  useEffect(() => {
    const theme = prefs.theme || PREFS_DEFAULTS.theme;
    if (theme === 'dark') {
      document.documentElement.setAttribute('data-theme', 'dark');
    } else {
      document.documentElement.removeAttribute('data-theme');
    }
  }, [prefs.theme]);

  // Auto-clear-done: every 10s, drop success jobs whose finishedAt is older
  // than the configured threshold. The 10s tick is coarse on purpose — the
  // shortest configurable threshold is 30s so we never miss the window.
  useEffect(() => {
    const seconds = Number(prefs['auto.clear_done.seconds'] || '0');
    if (!Number.isFinite(seconds) || seconds <= 0) return undefined;
    const thresholdMs = seconds * 1000;
    const id = setInterval(() => {
      const now = Date.now();
      setJobs((js) => js.filter((j) => {
        if (j.state !== 'success') return true;
        if (!j.finishedAt) return true;
        return (now - j.finishedAt) < thresholdMs;
      }));
    }, 10000);
    return () => clearInterval(id);
  }, [prefs]);

  const cleanupOrphans = useCallback(async () => {
    try {
      await invoke('kill_orphan_helpers', { pids: orphanPids });
      // Re-check after a moment.
      setTimeout(() => {
        invoke('find_orphan_helpers').then(setOrphanPids).catch(() => {});
      }, 500);
    } catch (e) {
      console.error('kill_orphan_helpers failed', e);
    }
  }, [orphanPids]);

  useEffect(() => {
    let mounted = true;
    const subs = [];

    listen('disk-cutter://job-update', (e) => {
      if (!mounted) return;
      setJobs((js) => applyJobUpdate(js, e.payload));
    }).then((u) => subs.push(u));

    listen('disk-cutter://job-complete', (e) => {
      if (!mounted) return;
      const finishedAt = Date.now();
      setJobs((js) => applyJobComplete(js, e.payload).map((j) => (
        j.id === e.payload.job_id && j.state === 'success'
          ? { ...j, finishedAt }
          : j
      )));
    }).then((u) => subs.push(u));

    listen('disk-cutter://job-error', (e) => {
      if (!mounted) return;
      const f = e.payload;
      setJobs((js) => applyJobFailure(js, f));
      if (f.error_code === 'ENEEDS_FDA') {
        invoke('open_fda_settings').catch((err) => console.error('open_fda_settings failed', err));
      }
    }).then((u) => subs.push(u));

    return () => {
      mounted = false;
      subs.forEach((u) => u());
    };
  }, []);

  const addImageFromPath = useCallback(async (path) => {
    try {
      const details = await invoke('inspect_image', { path });
      const image = {
        name: details.name,
        path: details.path,
        size: formatBytes(details.uncompressed_bytes),
        bytes: details.uncompressed_bytes,
        sectors: details.sectors,
        format: details.format,
        sha256: details.sha256 || '—',
      };
      setJobs((js) => [...js, makeJob(js.length + 1, image, null)]);
    } catch (e) {
      console.error('inspect_image failed', e);
      alert(t('error.could_not_add_image', { error: e }));
    }
  }, [t]);

  const addImage = useCallback(async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: 'Disk images', extensions: ['iso', 'img', 'bin', 'raw'] }],
    });
    if (!path) return;
    addImageFromPath(path);
  }, [addImageFromPath]);

  // Drag-and-drop disk images onto the window.
  useEffect(() => {
    let cleanup = null;
    getCurrentWindow().onDragDropEvent((event) => {
      if (event.payload.type === 'drop') {
        for (const path of event.payload.paths) {
          addImageFromPath(path);
        }
      }
    }).then((u) => { cleanup = u; });
    return () => { cleanup?.(); };
  }, [addImageFromPath]);

  // Escape closes the disk picker sheet.
  useEffect(() => {
    const onKey = (e) => { if (e.key === 'Escape') setPickerJob(null); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const startQueue = useCallback(async () => {
    const { tooSmall, okToBurn } = planStart(jobs);
    if (tooSmall.length) {
      setJobs((js) => js.map((j) => (
        tooSmall.some((t) => t.id === j.id) ? { ...j, state: 'error', errorCode: 'ETOOBIG' } : j
      )));
    }
    for (const job of okToBurn) {
      try {
        await invoke('start_write', {
          jobId: job.id,
          imagePath: job.image.path,
          targetDevice: job.target.device,
        });
      } catch (e) {
        console.error('start_write failed', e);
      }
    }
  }, [jobs]);

  const cancelJob = useCallback(async (jobId) => {
    try {
      await invoke('cancel_write', { jobId });
    } catch (e) {
      console.error('cancel_write failed', e);
    }
  }, []);

  const retryJob = useCallback(async (jobId) => {
    const job = jobs.find((j) => j.id === jobId);
    if (!job || !job.target) return;
    setJobs((js) => js.map((j) => (
      j.id !== jobId ? j : { ...j, state: 'idle', progress: 0, verifyProgress: 0, errorCode: undefined, errorMessage: undefined, verification: null }
    )));
    try {
      await invoke('start_write', {
        jobId: job.id,
        imagePath: job.image.path,
        targetDevice: job.target.device,
      });
    } catch (e) {
      console.error('retry start_write failed', e);
    }
  }, [jobs]);

  const clearDone = useCallback(() => {
    setJobs((js) => js.filter((j) => j.state !== 'success'));
  }, []);

  const removeJob = useCallback((jobId) => {
    setJobs((js) => js.filter((j) => j.id !== jobId));
  }, []);

  const flashAnother = useCallback((jobId) => {
    const job = jobs.find((j) => j.id === jobId);
    if (!job) return;
    setJobs((js) => [...js, makeJob(js.length + 1, job.image, null)]);
  }, [jobs]);

  const copyText = useCallback(async (text) => {
    if (!text) return;
    try { await navigator.clipboard.writeText(text); }
    catch (e) { console.error('clipboard write failed', e); }
  }, []);

  const pickTarget = useCallback((disk) => {
    setJobs((js) => js.map((j, i) => (i === pickerJob ? { ...j, target: disk } : j)));
    setPickerJob(null);
  }, [pickerJob]);

  const refreshDisks = useCallback(async () => {
    try { setDisks(await invoke('list_disks')); }
    catch (e) { console.error('list_disks failed', e); }
  }, []);

  const accent = tw.accent;
  const platform = tw.platform;
  // Density is now a prefs-driven setting. Fall back to the default until the
  // hydrate completes so first paint doesn't flicker between sizes.
  const density = prefs.density || PREFS_DEFAULTS.density;

  const errorJob = jobs.find((j) => j.state === 'error');
  const errorMsg = errorJob && errorJob.errorCode
    ? (ERROR_CODES.includes(errorJob.errorCode)
        ? { title: t(`error.${errorJob.errorCode}.title`), detail: t(`error.${errorJob.errorCode}.detail`) }
        : { title: t('error.generic'), detail: errorJob.errorMessage || '' })
    : null;

  // Sidebar nav can override the queue-derived scene (e.g. `prefs`) — the
  // titlebar/breadcrumbs still want the queue scene though so they don't
  // start blank-titled when the user wanders off-queue.
  const queueScene = computeScene(jobs, pickerJob, errorJob);
  const scene = activeNav === 'prefs' ? 'prefs'
    : activeNav === 'logs' ? 'logs'
    : queueScene;
  const titleText = t(sceneToTitleKey(queueScene, tw.verboseTitle));
  const useChrome = tw.decorations === 'custom';
  const visibleJobs = jobs;
  const sessionStats = computeSessionStats(jobs, sessionStartRef.current, Date.now());

  const bodyProps = {
    jobs, visibleJobs, accent, density, platform, scene, buildInfo, sessionStats,
    orphanPids, onCleanupOrphans: cleanupOrphans,
    prefs, onPrefChange: setPref,
    activeNav, setActiveNav,
    confirmed, setConfirmed,
    errorJob, errorMsg,
    expanded, setExpanded,
    setPickerJob,
    onAdd: addImage,
    onStart: startQueue,
    onCancelJob: cancelJob,
    onRetry: retryJob,
    onClearDone: clearDone,
    onFlashAnother: flashAnother,
    onCopyText: copyText,
    onRemoveJob: removeJob,
  };

  return (
    <div className={"stage" + (useChrome ? "" : " stage--native")} data-platform={platform} style={{ '--accent': accent }}>
      {useChrome ? (
        <WindowChrome platform={platform} title={titleText}>
          <AppBody {...bodyProps} />
        </WindowChrome>
      ) : (
        <div className="native-frame">
          <AppBody {...bodyProps} />
        </div>
      )}

      <DiskPickerSheet
        open={pickerJob !== null}
        disks={disks}
        jobImage={pickerJob !== null ? jobs[pickerJob]?.image : null}
        onPick={pickTarget}
        onClose={() => setPickerJob(null)}
        onRefresh={refreshDisks}
        accent={accent}
      />

      <TweaksPanel>
        <TweakSection label="TAURI">
          <TweakRadio label="Decorations" value={tw.decorations}
            options={[{ value: 'custom', label: 'custom' }, { value: 'native', label: 'native' }]}
            onChange={(v) => setTweak('decorations', v)} />
          <TweakRadio label="Platform" value={tw.platform}
            options={[{ value: 'mac', label: 'mac' }, { value: 'win', label: 'win' }, { value: 'lin', label: 'lin' }]}
            onChange={(v) => setTweak('platform', v)} />
        </TweakSection>
        <TweakSection label="CHROME">
          <TweakToggle label="Verbose title" value={tw.verboseTitle}
            onChange={(v) => setTweak('verboseTitle', v)} />
        </TweakSection>
        <TweakSection label="ACCENT">
          <TweakColor label="Hazard" value={tw.accent}
            options={ACCENT_OPTIONS}
            onChange={(v) => setTweak('accent', v)} />
        </TweakSection>
      </TweaksPanel>
    </div>
  );
}

function AppBody({
  jobs, visibleJobs, accent, density, platform, scene, buildInfo, sessionStats,
  orphanPids, onCleanupOrphans,
  prefs, onPrefChange,
  activeNav, setActiveNav,
  confirmed, setConfirmed,
  errorJob, errorMsg,
  expanded, setExpanded,
  setPickerJob,
  onAdd, onStart, onCancelJob,
  onRetry, onClearDone, onFlashAnother, onCopyText, onRemoveJob,
}) {
  const { t } = useTranslation();
  const writingCount = jobs.filter((j) => j.state === 'writing').length;
  const errorCount = jobs.filter((j) => j.state === 'error').length;
  const totalBytes = jobs.reduce((s, j) => s + (j.image?.bytes || 0), 0);
  const onPrefs = scene === 'prefs';
  const onLogs = scene === 'logs';
  const isQueueScene = !onPrefs && !onLogs;

  return (
    <div className={"app-shell density-" + density}>
      <Sidebar active={activeNav} onSelect={setActiveNav} jobs={jobs} accent={accent} sessionStats={sessionStats} />
      <main className="main">

        <header className="main-head">
          <div className="crumbs mono small">
            <span>{platform === 'mac' ? '~/' : platform === 'win' ? 'C:\\' : '/home/user/'}</span>
            <span className="crumbs-sep">▸</span>
            <span>{t('app.crumb_queue')}</span>
            <span className="crumbs-sep">▸</span>
            <span className="crumb-current">{t(`scene.${scene}.name`)}</span>
          </div>
          <h1 className="main-title">
            {scene === 'idle' && t('scene.idle.title')}
            {scene === 'writing' && (
              <Trans
                i18nKey="scene.writing.title"
                count={writingCount}
                values={{ count: writingCount }}
                components={{ 1: <span style={{ color: accent }} /> }}
              />
            )}
            {scene === 'verifying' && t('scene.verifying.title')}
            {scene === 'success' && t('scene.success.title', { count: jobs.length })}
            {scene === 'error' && t('scene.error.title', { count: errorCount })}
            {scene === 'diskpicker' && t('scene.diskpicker.title')}
            {scene === 'empty' && t('scene.empty.title')}
            {scene === 'prefs' && t('scene.prefs.title')}
            {scene === 'logs' && t('scene.logs.title')}
          </h1>
          <div className="main-sub mono">
            {t(`scene.${scene}.sub`)}
          </div>
        </header>

        {orphanPids && orphanPids.length > 0 && (
          <div className="test-mode-strip">
            <span className="test-mode-dot" />
            <span className="test-mode-label">
              {t('orphan.label', { count: orphanPids.length, pids: orphanPids.join(', ') })}
            </span>
            <button className="test-mode-btn" onClick={onCleanupOrphans}>{t('orphan.cleanup')}</button>
          </div>
        )}

        {errorJob && errorMsg && (
          <div className="error-strip" style={{ background: accent }}>
            <div className="error-strip-icon">✕</div>
            <div className="error-strip-body">
              <div className="error-strip-title">{errorMsg.title}</div>
              <div className="error-strip-detail">{errorMsg.detail}</div>
              {errorJob.errorMessage && (
                <div className="error-strip-raw mono">{errorJob.errorMessage}</div>
              )}
            </div>
            <div className="error-strip-code mono">{errorJob.errorCode}</div>
          </div>
        )}

        {isQueueScene && (
          <DangerBanner
            confirmed={confirmed}
            onConfirm={setConfirmed}
            jobs={jobs}
            accent={accent}
          />
        )}

        {isQueueScene && (
          <Toolbar
            onAdd={onAdd}
            onStart={onStart}
            onClearDone={onClearDone}
            confirmed={confirmed}
            jobs={jobs}
            accent={accent}
            busy={scene === 'writing' || scene === 'verifying'}
          />
        )}

        {onPrefs ? (
          <div className="prefs-scroll">
            <PrefsView values={prefs} onChange={onPrefChange} />
          </div>
        ) : onLogs ? (
          <LogsView accent={accent} />
        ) : visibleJobs.length === 0 ? (
          <EmptyState accent={accent} />
        ) : (
          <div className="queue">
            <div className="queue-head mono small">
              <span>{t('queue.head.num')}</span>
              <span>{t('queue.head.image')}</span>
              <span />
              <span>{t('queue.head.target')}</span>
              <span>{t('queue.head.state')}</span>
              <span>{t('queue.head.progress')}</span>
              <span />
              <span />
            </div>
            {visibleJobs.map((job) => (
              <JobRow
                key={job.id}
                job={job}
                accent={accent}
                density={density}
                expanded={!!expanded[job.id]}
                onToggle={() => setExpanded((e) => ({ ...e, [job.id]: !e[job.id] }))}
                onSelectTarget={() => setPickerJob(jobs.indexOf(job))}
                onCancel={() => onCancelJob(job.id)}
                onCopyHash={() => onCopyText(job.verification?.sourceHash)}
                onCopyError={() => onCopyText(job.errorMessage || job.errorCode)}
                onFlashAnother={() => onFlashAnother(job.id)}
                onRetry={() => onRetry(job.id)}
                onRemove={() => onRemoveJob(job.id)}
              />
            ))}
          </div>
        )}

        <footer className="main-foot mono small">
          <div className="foot-cell">
            <span>{t('footer.total')}</span>
            <b>{t('footer.jobs', { count: jobs.length })}</b>
          </div>
          <div className="foot-cell">
            <span>{t('footer.data')}</span>
            <b>{(totalBytes / 1e9).toFixed(2)} GB</b>
          </div>
          <div className="foot-cell">
            <span>{t('footer.status')}</span>
            <b style={{ color: errorJob ? accent : 'var(--ink)' }}>
              {errorJob ? t('footer.status_fault')
                : scene === 'success' ? t('footer.status_idle')
                : scene === 'writing' || scene === 'verifying' ? t('footer.status_running')
                : t('footer.status_ready')}
            </b>
          </div>
          <div className="foot-spacer" />
          <div className="foot-cell foot-cell--mono">
            <span>{t('footer.build')}</span>
            <b>{buildInfo}</b>
          </div>
        </footer>
      </main>
    </div>
  );
}

function EmptyState({ accent }) {
  const { t } = useTranslation();
  return (
    <div className="empty-state">
      <div className="empty-glyph" style={{ borderColor: 'var(--ink)' }}>
        <div className="empty-stripes" />
        <div className="empty-label mono">{t('empty.default_label')}</div>
      </div>
      <div className="empty-help mono">
        {t('empty.help_pre')}<b style={{ color: accent }}>{t('empty.help_button')}</b>{t('empty.help_post')}
      </div>
    </div>
  );
}

export default App;
