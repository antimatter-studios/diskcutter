import React from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import i18n, { availableLanguages } from './i18n/index.js';
import { formatBytes, formatBps, formatDuration } from './format.js';

const winCtl = {
  minimize: () => getCurrentWindow().minimize(),
  toggleMax: () => getCurrentWindow().toggleMaximize(),
  close: () => getCurrentWindow().close(),
};

// components.jsx — Disk Cutter
// Brutalist UI primitives: window chrome (mac/win/linux), banner, job row,
// disk picker sheet, sidebar, hash readouts, progress bars.

/* ─────────── Window Chrome ─────────── */

function WindowChrome({ platform, title, children }) {
  // Shared shell: 2px ink border, sharp corners, drop shadow.
  const chrome = {
    mac: <MacBar title={title} />,
    win: <WinBar title={title} />,
    lin: <LinBar title={title} />,
  }[platform];
  return (
    <div className="window">
      {chrome}
      <div className="window-body">{children}</div>
    </div>
  );
}

function MacBar({ title }) {
  return (
    <div className="titlebar titlebar--mac" data-tauri-drag-region>
      <div className="tl-mac">
        <i style={{ background: 'var(--ctl-close)' }} onClick={winCtl.close} title="Close" />
        <i style={{ background: 'var(--ctl-min)' }} onClick={winCtl.minimize} title="Minimize" />
        <i style={{ background: 'var(--ctl-zoom)' }} onClick={winCtl.toggleMax} title="Zoom" />
      </div>
      <div className="title-text" data-tauri-drag-region>{title}</div>
      <div className="tl-spacer" data-tauri-drag-region />
    </div>
  );
}

function WinBar({ title }) {
  return (
    <div className="titlebar titlebar--win" data-tauri-drag-region>
      <div className="title-text title-text--win" data-tauri-drag-region>{title}</div>
      <div className="win-controls">
        <button onClick={winCtl.minimize}>—</button>
        <button onClick={winCtl.toggleMax}>▢</button>
        <button onClick={winCtl.close}>✕</button>
      </div>
    </div>
  );
}

function LinBar({ title }) {
  return (
    <div className="titlebar titlebar--lin" data-tauri-drag-region>
      <div className="title-text title-text--lin" data-tauri-drag-region>{title}</div>
      <div className="lin-controls">
        <button onClick={winCtl.minimize}>_</button>
        <button onClick={winCtl.toggleMax}>□</button>
        <button onClick={winCtl.close}>×</button>
      </div>
    </div>
  );
}

/* ─────────── Sidebar ─────────── */

function Sidebar({ active, onSelect, jobs, accent, sessionStats }) {
  const { t } = useTranslation();
  const failedCount = jobs.filter(j => j.state === 'error').length;
  const counts = {
    queue: jobs.length,
  };
  const stats = sessionStats || { session: '—', written: '—', avg: '—' };
  return (
    <aside className="sidebar">
      <div className="logo">
        <div className="logo-mark" style={{ background: accent, color: 'var(--ink)' }}>
          <svg viewBox="0 0 24 24" width="20" height="20"><circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" strokeWidth="2"/><circle cx="12" cy="12" r="3" fill="currentColor"/></svg>
        </div>
        <div className="logo-text">
          <div className="logo-name">{t('app.logo_name_line1')}<br/>{t('app.logo_name_line2')}</div>
          <div className="logo-ver">v0.4.0-alpha</div>
        </div>
      </div>

      <nav className="nav">
        <SideItem k="queue" label={t('sidebar.nav.queue')} count={counts.queue} active={active==='queue'} onClick={onSelect} accent={accent} hazard={failedCount > 0} />
        <SideItem k="logs"  label={t('sidebar.nav.logs')}  active={active==='logs'} onClick={onSelect} />
        <SideItem k="prefs" label={t('sidebar.nav.prefs')} active={active==='prefs'} onClick={onSelect} />
      </nav>

      <div className="side-foot">
        <div className="side-foot-row"><span>{t('sidebar.foot.session')}</span><b>{stats.session}</b></div>
        <div className="side-foot-row"><span>{t('sidebar.foot.written')}</span><b>{stats.written}</b></div>
        <div className="side-foot-row"><span>{t('sidebar.foot.avg')}</span><b>{stats.avg}</b></div>
      </div>
    </aside>
  );
}

function SideItem({ k, label, count, active, onClick, accent, hazard }) {
  return (
    <button className={"side-item" + (active ? " is-active" : "")} onClick={() => onClick(k)}>
      <span className="side-tick">{active ? "▶" : ""}</span>
      <span className="side-label">{label}</span>
      {count != null && (
        <span className="side-count" style={hazard ? { background: accent, color: 'var(--on-accent)' } : {}}>
          {String(count).padStart(2, '0')}
        </span>
      )}
    </button>
  );
}

/* ─────────── Danger Banner ─────────── */

function DangerBanner({ confirmed, onConfirm, jobs, accent }) {
  const { t } = useTranslation();
  // Banner is purely a pre-flight warning + confirmation gate. Once the user
  // has clicked START there are no `idle` jobs left to confirm — hide it.
  const idleWithTarget = jobs.filter(j => j.state === 'idle' && j.target).length;
  if (idleWithTarget === 0) return null;
  const targets = idleWithTarget;
  return (
    <div className="banner" style={{ '--hazard': accent }}>
      <div className="banner-stripes" />
      <div className="banner-body">
        <div className="banner-icon">⚠</div>
        <div className="banner-text">
          <div className="banner-headline">
            <Trans
              i18nKey="banner.headline"
              count={targets}
              values={{ count: targets }}
              components={{ 1: <span /> }}
            />
          </div>
          <div className="banner-sub">{t('banner.sub')}</div>
        </div>
        <label className={"banner-check" + (confirmed ? " is-on" : "")}>
          <input type="checkbox" checked={confirmed} onChange={(e) => onConfirm(e.target.checked)} />
          <span className="banner-box">{confirmed ? "✕" : ""}</span>
          <span className="banner-cta">{t('banner.cta_line1')}<br/>{t('banner.cta_line2')}</span>
        </label>
      </div>
    </div>
  );
}

/* ─────────── Toolbar ─────────── */

function Toolbar({ onAdd, onStart, onClearDone, confirmed, jobs, accent, busy }) {
  const { t } = useTranslation();
  const ready = confirmed && jobs.some(j => j.state === 'idle' && j.target);
  const hasDone = jobs.some(j => j.state === 'success');
  return (
    <div className="toolbar">
      <div className="toolbar-left">
        <button className="btn btn-ghost" onClick={onAdd}>
          <span className="btn-bracket">[</span> {t('toolbar.add_image')} <span className="btn-bracket">]</span>
        </button>
        <div className="tb-sep" />
        <button className={"btn btn-ghost" + (hasDone ? "" : " is-disabled")} onClick={hasDone ? onClearDone : null}>[ {t('toolbar.clear_done')} ]</button>
      </div>
      <div className="toolbar-right">
        <button
          className={"btn btn-primary" + (ready ? "" : " is-disabled")}
          style={{ background: ready ? accent : 'var(--disabled)' }}
          onClick={ready ? onStart : null}>
          {busy ? `▣ ${t('toolbar.running')}` : `▶ ${t('toolbar.start_queue')}`}
        </button>
      </div>
    </div>
  );
}

/* ─────────── Job Row ─────────── */

function JobRow({ job, accent, expanded, onToggle, onSelectTarget, onCancel, onRetry, onCopyHash, onCopyError, onFlashAnother, onRemove, density }) {
  const { t } = useTranslation();
  const state = job.state;
  const danger = state === 'error';
  const writing = state === 'writing';
  const verifying = state === 'verifying';
  const success = state === 'success';

  return (
    <div className={"job" + (danger ? " job--danger" : "") + (expanded ? " job--open" : "") + (density === 'compact' ? " job--compact" : "")}
         style={{ '--accent': accent }}>
      <div className="job-head" onClick={onToggle}>
        <div className="job-num">#{job.num.toString().padStart(2,'0')}</div>

        <div className="job-image">
          <div className="job-image-name">{job.image.name}</div>
          <div className="job-image-meta">
            <span>{job.image.size}</span>
            <span className="dot">·</span>
            <span className="mono small">sha256: {job.image.sha256.slice(0,12)}…{job.image.sha256.slice(-4)}</span>
          </div>
        </div>

        <div className="job-arrow">
          {state === 'idle' ? '──→' : writing ? '═══►' : verifying ? '─◇─►' : success ? '═══►' : '─⨯─→'}
        </div>

        <div className="job-target">
          {job.target ? (
            <>
              <div className="job-target-name">{job.target.model}</div>
              <div className="job-target-meta">
                <span>{job.target.capacity}</span>
                <span className="dot">·</span>
                <span className="mono small">{job.target.bus}</span>
              </div>
            </>
          ) : (
            <button className="pick-target" onClick={(e) => { e.stopPropagation(); onSelectTarget(); }}>
              [ {t('job.select_target')} ]
            </button>
          )}
        </div>

        <div className="job-state">
          <StateGlyph state={state} accent={accent} />
        </div>

        <div className="job-progress">
          {state === 'idle' && job.target && <div className="status-tag">{t('job.state.ready')}</div>}
          {state === 'idle' && !job.target && <div className="status-tag faint">{t('job.state.awaiting_target')}</div>}
          {writing && <ProgressBar value={job.progress} label={t('job.state.write')} speed={job.speed} eta={job.eta} accent={accent} />}
          {verifying && <ProgressBar value={job.verifyProgress} label={t('job.state.verify')} speed={job.speed} eta={job.eta} accent="var(--ink)" />}
          {success && <SuccessReadout job={job} />}
          {danger && <div className="status-tag status-tag--danger">{job.errorCode}</div>}
        </div>

        <button className="job-chev" onClick={(e) => { e.stopPropagation(); onToggle(); }}>
          {expanded ? "▼" : "▶"}
        </button>

        {(writing || verifying) ? (
          <span className="job-remove job-remove--disabled" aria-hidden="true" />
        ) : (
          <button
            className="job-remove"
            title={t('job.remove')}
            aria-label={t('job.remove')}
            onClick={(e) => { e.stopPropagation(); onRemove(); }}
          >
            ✕
          </button>
        )}
      </div>

      {expanded && <JobDetail job={job} accent={accent} onCancel={onCancel} onRetry={onRetry} onCopyHash={onCopyHash} onCopyError={onCopyError} onFlashAnother={onFlashAnother} />}
    </div>
  );
}

function StateGlyph({ state, accent }) {
  if (state === 'success') return <div className="glyph glyph--ok">✓</div>;
  if (state === 'error') return <div className="glyph glyph--err" style={{ background: accent }}>✕</div>;
  if (state === 'writing') return <div className="glyph glyph--run">▣</div>;
  if (state === 'verifying') return <div className="glyph glyph--run">◇</div>;
  return <div className="glyph">·</div>;
}

function ProgressBar({ value, label, speed, eta, accent }) {
  const { t } = useTranslation();
  return (
    <div className="pb">
      <div className="pb-meta">
        <span className="pb-label">{label}</span>
        <span className="pb-pct">{value.toFixed(1)}%</span>
        <span className="pb-stat">{speed}</span>
        <span className="pb-stat">{t('job.eta', { value: eta })}</span>
      </div>
      <div className="pb-track">
        <div className="pb-fill" style={{ width: `${value}%`, background: accent }} />
        <div className="pb-hatch" style={{ width: `${value}%` }} />
      </div>
    </div>
  );
}

function SuccessReadout({ job }) {
  const { t } = useTranslation();
  return (
    <div className="pb">
      <div className="pb-meta">
        <span className="pb-label">{t('job.state.done')}</span>
        <span className="pb-stat">{job.elapsed}</span>
        <span className="pb-stat">{t('verify.throughput_value', { value: job.speed })}</span>
        <span className="pb-stat">{t('job.state.sha_match')}</span>
      </div>
      <div className="pb-track">
        <div className="pb-fill" style={{ width: '100%', background: 'var(--ink)' }} />
      </div>
    </div>
  );
}

/* ─────────── Job Detail (expanded drawer) ─────────── */

function JobDetail({ job, accent, onCancel, onRetry }) {
  const { t } = useTranslation();
  return (
    <div className="job-detail">
      <div className="detail-grid">
        <DetailBlock label={t('detail.block.image')}>
          <KV k={t('detail.kv.path')} v={job.image.path} mono />
          <KV k={t('detail.kv.size')} v={t('detail.kv.size_value', { size: job.image.size, bytes: job.image.bytes.toLocaleString() })} mono />
          <KV k={t('detail.kv.sectors')} v={job.image.sectors.toLocaleString()} mono />
          <KV k={t('detail.kv.format')} v={job.image.format} />
          <KV k={t('detail.kv.sha256_source')} v={job.image.sha256} mono wrap />
        </DetailBlock>

        <DetailBlock label={t('detail.block.target')}>
          {job.target ? (
            <>
              <KV k={t('detail.kv.device')} v={job.target.device} mono />
              <KV k={t('detail.kv.model')} v={job.target.model} />
              <KV k={t('detail.kv.capacity')} v={t('detail.kv.capacity_value', { capacity: job.target.capacity, bytes: job.target.bytes.toLocaleString() })} mono />
              <KV k={t('detail.kv.bus')} v={job.target.bus} mono />
              <KV k={t('detail.kv.partitions')} v={job.target.partitions} />
            </>
          ) : <div className="empty-line">{t('detail.kv.no_target')}</div>}
        </DetailBlock>

        <DetailBlock label={t('detail.block.verification')} full>
          <VerificationPanel job={job} accent={accent} />
        </DetailBlock>
      </div>

      <div className="detail-actions">
        {job.state === 'writing' || job.state === 'verifying' ? (
          <button className="btn btn-danger" style={{ borderColor: accent, color: accent }} onClick={onCancel}>
            [ {t('detail.actions.abort')} ]
          </button>
        ) : null}
        {job.state === 'error' && (
          <>
            <button className="btn btn-ghost" onClick={onRetry}>[ {t('detail.actions.retry')} ]</button>
            <button className="btn btn-ghost">[ {t('detail.actions.copy_error')} ]</button>
            <button className="btn btn-ghost">[ {t('detail.actions.open_log')} ]</button>
          </>
        )}
        {job.state === 'success' && (
          <>
            <button className="btn btn-ghost">[ {t('detail.actions.eject')} ]</button>
            <button className="btn btn-ghost">[ {t('detail.actions.copy_hash')} ]</button>
            <button className="btn btn-ghost">[ {t('detail.actions.flash_another')} ]</button>
          </>
        )}
      </div>
    </div>
  );
}

function DetailBlock({ label, full, children }) {
  return (
    <div className={"detail-block" + (full ? " detail-block--full" : "")}>
      <div className="detail-label">▌ {label}</div>
      <div className="detail-body">{children}</div>
    </div>
  );
}

function KV({ k, v, mono, wrap }) {
  return (
    <div className="kv">
      <div className="kv-k">{k}</div>
      <div className={"kv-v" + (mono ? " mono" : "") + (wrap ? " wrap" : "")}>{v}</div>
    </div>
  );
}

/* ─────────── Verification panel ─────────── */

function VerificationPanel({ job, accent }) {
  const { t } = useTranslation();
  const state = job.state;
  const ver = job.verification;
  if (!ver) {
    return <div className="empty-line">{t('verify.runs_after_write')}</div>;
  }
  return (
    <div className="ver">
      <div className="ver-hashes">
        <div className="ver-hash">
          <div className="ver-hash-k">{t('verify.source')}</div>
          <div className="mono hash">{ver.sourceHash}</div>
        </div>
        <div className="ver-hash">
          <div className="ver-hash-k">{t('verify.readback')}</div>
          <div className={"mono hash" + (ver.match ? "" : " hash--bad")}>{ver.readHash}</div>
        </div>
        <div className={"ver-verdict" + (ver.match ? " ver-verdict--ok" : " ver-verdict--bad")}
             style={!ver.match ? { background: accent } : {}}>
          {ver.match ? t('verify.hashes_match') : t('verify.hash_mismatch')}
        </div>
      </div>

      <div className="ver-stats">
        <Stat k={t('verify.sectors_checked')} v={`${ver.checked.toLocaleString()} / ${ver.total.toLocaleString()}`} />
        <Stat k={t('verify.block_size')} v="512 B" />
        <Stat k={t('verify.mismatches')} v={ver.mismatches.length.toString().padStart(3,'0')} bad={ver.mismatches.length > 0} accent={accent} />
        <Stat k={t('verify.throughput')} v={ver.throughput} />
      </div>

      {ver.mismatches.length > 0 && (
        <div className="ver-mismatches">
          <div className="ver-mismatches-head">▌ {t('verify.mismatched_sectors')}</div>
          <table className="ver-table">
            <thead>
              <tr><th>{t('verify.col.lba')}</th><th>{t('verify.col.offset')}</th><th>{t('verify.col.expected')}</th><th>{t('verify.col.actual')}</th><th>{t('verify.col.note')}</th></tr>
            </thead>
            <tbody>
              {ver.mismatches.map((m, i) => (
                <tr key={i}>
                  <td className="mono">{m.lba}</td>
                  <td className="mono">{m.offset}</td>
                  <td className="mono">{m.expected}</td>
                  <td className="mono" style={{ color: accent }}>{m.actual}</td>
                  <td>{m.note}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {state === 'verifying' && (
        <SectorMap progress={job.verifyProgress} accent={accent} />
      )}
    </div>
  );
}

function Stat({ k, v, bad, accent }) {
  return (
    <div className="stat">
      <div className="stat-k">{k}</div>
      <div className="stat-v" style={bad ? { color: accent } : {}}>{v}</div>
    </div>
  );
}

function SectorMap({ progress, accent }) {
  const { t } = useTranslation();
  // 64 cells; light up cells up to progress%
  const cells = 64;
  const lit = Math.floor((progress / 100) * cells);
  return (
    <div className="sectormap">
      <div className="sectormap-head">
        <span>{t('sectormap.title')}</span>
        <span className="mono small">{t('sectormap.blocks_scanned', { lit, total: cells })}</span>
      </div>
      <div className="sectormap-grid">
        {Array.from({ length: cells }).map((_, i) => (
          <div key={i}
            className={"cell" + (i < lit ? " cell--lit" : "") + (i === lit ? " cell--cursor" : "")}
            style={i < lit ? { background: 'var(--ink)' } : i === lit ? { background: accent } : {}}
          />
        ))}
      </div>
    </div>
  );
}

/* ─────────── Disk picker sheet ─────────── */

function DiskPickerSheet({ open, disks, jobImage, onPick, onClose, onRefresh, accent }) {
  const { t } = useTranslation();
  const [refreshedAt, setRefreshedAt] = React.useState(Date.now());
  const [, tick] = React.useState(0);
  React.useEffect(() => {
    if (!open) return;
    const i = setInterval(() => tick(n => n + 1), 1000);
    return () => clearInterval(i);
  }, [open]);
  const onRefreshClick = async () => {
    if (onRefresh) { await onRefresh(); setRefreshedAt(Date.now()); }
  };
  if (!open) return null;
  const seconds = Math.floor((Date.now() - refreshedAt) / 1000);
  return (
    <div className="sheet-backdrop" onClick={onClose}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-head">
          <div>
            <div className="sheet-eyebrow">{t('picker.eyebrow')}</div>
            <div className="sheet-title">
              <Trans
                i18nKey="picker.title"
                values={{ name: jobImage?.name || "—" }}
                components={{ 1: <b /> }}
              />
            </div>
          </div>
          <button className="sheet-x" onClick={onClose}>✕</button>
        </div>

        <div className="sheet-warning">
          <span style={{ color: accent }}>⚠</span>
          {t('picker.warning')}
        </div>

        <div className="disk-list">
          {disks.map((d, i) => {
            const tooSmall = jobImage && d.bytes < jobImage.bytes;
            const isInternal = d.bus.includes('NVME') || d.flags?.includes('INTERNAL');
            return (
              <button key={d.device} className={"disk" + (tooSmall ? " disk--small" : "") + (isInternal ? " disk--system" : "")}
                      onClick={() => !tooSmall && !isInternal && onPick(d)}>
                <div className="disk-icon">{isInternal ? '⛔' : '⬚'}</div>
                <div className="disk-body">
                  <div className="disk-row1">
                    <span className="disk-model">{d.model}</span>
                    {isInternal && <span className="disk-flag">{t('picker.system_disk')}</span>}
                    {tooSmall && <span className="disk-flag" style={{ color: accent }}>{t('picker.too_small')}</span>}
                  </div>
                  <div className="disk-row2 mono small">
                    <span>{d.device}</span>
                    <span className="dot">·</span>
                    <span>{d.capacity}</span>
                    <span className="dot">·</span>
                    <span>{d.bus}</span>
                    <span className="dot">·</span>
                    <span>{d.partitions}</span>
                  </div>
                </div>
                <div className="disk-pick">{isInternal || tooSmall ? '—' : `[ ${t('picker.pick')} ]`}</div>
              </button>
            );
          })}
        </div>

        <div className="sheet-foot mono small">
          <span>{t('picker.disks_detected', { count: disks.length })}</span>
          <span>·</span>
          <span>{t('picker.refreshed', { seconds })}</span>
          <span style={{ marginLeft: 'auto' }}>
            <button className="picker-link" onClick={onRefreshClick}>{t('picker.refresh')}</button>
            &nbsp;&nbsp;
            <button className="picker-link" onClick={onClose}>{t('picker.cancel')}</button>
          </span>
        </div>
      </div>
    </div>
  );
}

/* ─────────── Prefs view ─────────── */

// Schema for every config key the UI manages. The order here drives render
// order within each section.
const PREFS_SECTIONS = [
  {
    key: 'performance',
    fields: [
      { key: 'writer.impl', type: 'select',
        options: [
          { value: 'raw', labelKey: 'prefs.writer_impl.raw' },
          { value: 'block', labelKey: 'prefs.writer_impl.block' },
          { value: 'pipelined', labelKey: 'prefs.writer_impl.pipelined' },
        ] },
      { key: 'chunk.bytes', type: 'select',
        options: [
          { value: '262144',   label: '256 KiB' },
          { value: '524288',   label: '512 KiB' },
          { value: '1048576',  label: '1 MiB' },
          { value: '2097152',  label: '2 MiB' },
          { value: '4194304',  label: '4 MiB' },
          { value: '8388608',  label: '8 MiB' },
          { value: '16777216', label: '16 MiB' },
        ] },
      { key: 'workers.count', type: 'select',
        options: ['1','2','4','8','16'].map((v) => ({ value: v, label: v })) },
      { key: 'queue.depth', type: 'select',
        options: ['4','8','15','32','64'].map((v) => ({ value: v, label: v })) },
      { key: 'verify.skip', type: 'toggle' },
      { key: 'hash.algo', type: 'select',
        options: [
          { value: 'sha256', label: 'sha256' },
          { value: 'xxhash', label: 'xxhash' },
        ] },
      { key: 'max.mismatches', type: 'select',
        options: ['16','64','256','1024'].map((v) => ({ value: v, label: v })) },
    ],
  },
  {
    key: 'display',
    fields: [
      { key: 'language', type: 'language' },
      { key: 'theme', type: 'select',
        options: [
          { value: 'light', labelKey: 'prefs.theme.light' },
          { value: 'dark',  labelKey: 'prefs.theme.dark' },
        ] },
      { key: 'density', type: 'select',
        options: [
          { value: 'compact', labelKey: 'prefs.density.compact' },
          { value: 'comfy',   labelKey: 'prefs.density.comfy' },
        ] },
    ],
  },
  {
    key: 'behavior',
    fields: [
      { key: 'auto.eject', type: 'toggle' },
      { key: 'auto.clear_done.seconds', type: 'select',
        options: [
          { value: '0',   labelKey: 'prefs.auto_clear_done.off' },
          { value: '30',  labelKey: 'prefs.auto_clear_done.30s' },
          { value: '60',  labelKey: 'prefs.auto_clear_done.60s' },
          { value: '300', labelKey: 'prefs.auto_clear_done.5m' },
          { value: '600', labelKey: 'prefs.auto_clear_done.10m' },
        ] },
    ],
  },
];

// Keep this in sync with App.jsx — both consult the same defaults when
// hydrating from a half-populated config table.
const PREFS_DEFAULTS = {
  'writer.impl': 'pipelined',
  'chunk.bytes': '1048576',
  'workers.count': '4',
  'queue.depth': '15',
  'verify.skip': 'false',
  'hash.algo': 'sha256',
  'max.mismatches': '256',
  'language': '',
  'theme': 'light',
  'density': 'comfy',
  'auto.eject': 'false',
  'auto.clear_done.seconds': '0',
};

function prefsLabelKey(configKey) {
  // "writer.impl" → "prefs.label.writer_impl"
  return 'prefs.label.' + configKey.replaceAll('.', '_');
}

function PrefsView({ values, onChange }) {
  const { t } = useTranslation();
  return (
    <div className="prefs">
      {PREFS_SECTIONS.map((sect) => (
        <div key={sect.key} className="detail-block prefs-block">
          <div className="detail-label">▌ {t('prefs.section.' + sect.key)}</div>
          <div className="prefs-rows">
            {sect.fields.map((f) => (
              <PrefsRow
                key={f.key}
                field={f}
                value={values[f.key] ?? PREFS_DEFAULTS[f.key] ?? ''}
                onChange={(v) => onChange(f.key, v)}
              />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

function PrefsRow({ field, value, onChange }) {
  const { t } = useTranslation();
  const label = t(prefsLabelKey(field.key));
  return (
    <div className="prefs-row">
      <div className="prefs-row-k mono">{label}</div>
      <div className="prefs-row-v">
        <PrefsControl field={field} value={value} onChange={onChange} />
      </div>
    </div>
  );
}

function PrefsControl({ field, value, onChange }) {
  const { t } = useTranslation();
  if (field.type === 'toggle') {
    const on = value === 'true';
    return (
      <button
        type="button"
        className={"prefs-toggle" + (on ? " is-on" : "")}
        role="switch"
        aria-checked={on}
        onClick={() => onChange(on ? 'false' : 'true')}
      >
        <span className="prefs-toggle-track">
          <span className="prefs-toggle-thumb" />
        </span>
        <span className="prefs-toggle-label mono">
          {on ? t('prefs.toggle.on') : t('prefs.toggle.off')}
        </span>
      </button>
    );
  }
  if (field.type === 'language') {
    return (
      <select
        className="prefs-select mono"
        value={value || i18n.language}
        onChange={(e) => onChange(e.target.value)}
      >
        {availableLanguages.map((l) => (
          <option key={l.code} value={l.code}>{l.name}</option>
        ))}
      </select>
    );
  }
  return (
    <select
      className="prefs-select mono"
      value={value}
      onChange={(e) => onChange(e.target.value)}
    >
      {field.options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.labelKey ? t(o.labelKey) : o.label}
        </option>
      ))}
    </select>
  );
}

/* ─────────── Logs view ─────────── */

function formatLogTimestamp(ms) {
  if (!ms) return '—';
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function formatRelative(ms, t) {
  if (!ms) return '—';
  const diff = Math.max(0, Date.now() - ms);
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return t('logs.relative.just_now');
  const min = Math.floor(sec / 60);
  if (min < 60) return t('logs.relative.minutes', { count: min });
  const hr = Math.floor(min / 60);
  if (hr < 24) return t('logs.relative.hours', { count: hr });
  const day = Math.floor(hr / 24);
  return t('logs.relative.days', { count: day });
}

function shortDevice(device) {
  if (!device) return '—';
  return device.replace(/^\/dev\//, '');
}

function LogsView({ accent }) {
  const { t } = useTranslation();
  const [burns, setBurns] = React.useState([]);
  const [selectedId, setSelectedId] = React.useState(null);
  const [logs, setLogs] = React.useState([]);
  const [loading, setLoading] = React.useState(true);
  const [logsLoading, setLogsLoading] = React.useState(false);
  const [error, setError] = React.useState(null);
  const tailRef = React.useRef(null);
  const selectedRef = React.useRef(null);

  React.useEffect(() => { selectedRef.current = selectedId; }, [selectedId]);

  const refresh = React.useCallback(async () => {
    try {
      const rows = await invoke('burn_history_list', { limit: 500 });
      setBurns(rows);
      setError(null);
      if (selectedRef.current == null && rows.length) {
        setSelectedId(rows[0].id);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadLogs = React.useCallback(async (id) => {
    if (id == null) { setLogs([]); return; }
    setLogsLoading(true);
    try {
      const rows = await invoke('burn_logs_list', { burnId: id });
      setLogs(rows);
    } catch (e) {
      setError(String(e));
    } finally {
      setLogsLoading(false);
    }
  }, []);

  React.useEffect(() => { refresh(); }, [refresh]);
  React.useEffect(() => { loadLogs(selectedId); }, [selectedId, loadLogs]);

  // Burn-job lifecycle events fire when a row in burn_history is created or
  // updated, so refresh both the list and the visible log lines.
  React.useEffect(() => {
    let mounted = true;
    const subs = [];
    const onActivity = () => {
      if (!mounted) return;
      refresh();
      const id = selectedRef.current;
      if (id != null) loadLogs(id);
    };
    listen('disk-cutter://job-update', onActivity).then((u) => subs.push(u));
    listen('disk-cutter://job-complete', onActivity).then((u) => subs.push(u));
    listen('disk-cutter://job-error', onActivity).then((u) => subs.push(u));
    return () => { mounted = false; subs.forEach((u) => u()); };
  }, [refresh, loadLogs]);

  React.useEffect(() => {
    if (tailRef.current) tailRef.current.scrollTop = tailRef.current.scrollHeight;
  }, [logs]);

  const selected = burns.find((b) => b.id === selectedId) || null;

  const onClear = async () => {
    if (!burns.length) return;
    if (!window.confirm(t('logs.confirm_clear'))) return;
    try {
      await invoke('burn_history_clear');
      setSelectedId(null);
      setLogs([]);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="logs-view">
      <div className="log-history">
        <div className="log-history-head mono small">
          <span>{t('logs.history.heading')}</span>
          <button className="picker-link" onClick={() => refresh()}>
            {t('logs.refresh')}
          </button>
        </div>
        {loading ? (
          <div className="log-empty mono small">{t('logs.loading')}</div>
        ) : burns.length === 0 ? (
          <div className="log-empty mono small">{t('logs.history.empty')}</div>
        ) : (
          <div className="log-history-list">
            {burns.map((b) => (
              <button
                key={b.id}
                className={"log-history-row" + (b.id === selectedId ? " is-active" : "")}
                onClick={() => setSelectedId(b.id)}
              >
                <div className="log-history-row1">
                  <span className="log-history-name">{b.image_name || '—'}</span>
                  <BurnStateBadge state={b.state} accent={accent} />
                </div>
                <div className="log-history-row2 mono small">
                  <span>→ {shortDevice(b.target_device)}</span>
                  <span className="dot">·</span>
                  <span>{formatBytes(b.image_bytes)}</span>
                  <span className="dot">·</span>
                  <span>{formatRelative(b.started_at, t)}</span>
                </div>
              </button>
            ))}
          </div>
        )}
        <div className="log-history-foot">
          <button
            className={"btn btn-ghost" + (burns.length ? "" : " is-disabled")}
            onClick={burns.length ? onClear : null}
          >
            [ {t('logs.clear_history')} ]
          </button>
        </div>
      </div>

      <div className="log-detail">
        {error && (
          <div className="log-error mono small" style={{ background: accent }}>
            {error}
          </div>
        )}
        {!selected ? (
          <div className="log-empty log-empty--big mono">
            {burns.length ? t('logs.detail.pick') : t('logs.detail.empty')}
          </div>
        ) : (
          <>
            <div className="log-detail-head">
              <div className="log-detail-title">{selected.image_name || '—'}</div>
              <div className="log-detail-sub mono small">
                <span>→ {selected.target_device}</span>
                <span className="dot">·</span>
                <span>{formatBytes(selected.image_bytes)}</span>
                <span className="dot">·</span>
                <span>{formatLogTimestamp(selected.started_at)}</span>
                {selected.elapsed_ms != null && (
                  <>
                    <span className="dot">·</span>
                    <span>{formatDuration(selected.elapsed_ms)}</span>
                  </>
                )}
                {selected.avg_write_bps != null && (
                  <>
                    <span className="dot">·</span>
                    <span>{formatBps(selected.avg_write_bps)} {t('logs.detail.write_avg')}</span>
                  </>
                )}
              </div>
              {selected.error_code && (
                <div className="log-detail-err mono small" style={{ borderColor: accent, color: accent }}>
                  {selected.error_code}{selected.error_message ? `: ${selected.error_message}` : ''}
                </div>
              )}
            </div>

            <div className="log-lines mono" ref={tailRef}>
              {logsLoading && logs.length === 0 ? (
                <div className="log-empty small">{t('logs.loading')}</div>
              ) : logs.length === 0 ? (
                <div className="log-empty small">{t('logs.detail.no_lines')}</div>
              ) : (
                logs.map((l) => (
                  <div key={l.id} className={"log-line log-line--" + l.level}>
                    <span className="log-line-ts">{formatLogTimestamp(l.ts)}</span>
                    <span className="log-line-level">{l.level.toUpperCase()}</span>
                    <span className="log-line-msg">{l.message}</span>
                  </div>
                ))
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function BurnStateBadge({ state, accent }) {
  const { t } = useTranslation();
  const s = state || 'unknown';
  const label = t(`logs.state.${s}`, { defaultValue: s.toUpperCase() });
  const isErr = s === 'error';
  const style = isErr ? { background: accent, color: 'var(--on-accent)', borderColor: accent } : {};
  return (
    <span className={"log-state-badge log-state-badge--" + s} style={style}>{label}</span>
  );
}

export {
  WindowChrome, Sidebar, DangerBanner, Toolbar,
  JobRow, DiskPickerSheet, LogsView,
  PrefsView, PREFS_DEFAULTS,
};
