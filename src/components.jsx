import React from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import i18n, { availableLanguages } from './i18n/index.js';
import { formatBytes, formatBps, formatDuration } from './format.js';
import logoUrl from './assets/logo.svg';

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
        <div className="logo-mark">
          <img src={logoUrl} alt="" width="34" height="34" />
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

function Toolbar({
  onAdd, onAddFromUrl, onBrowseCatalog, onStart, onClearDone,
  confirmed, jobs, accent, busy,
  copies, onChangeCopies,
}) {
  const { t } = useTranslation();
  // Same gate as the Cmd+Enter shortcut path (see keymap.js::queueReady):
  // burn requires confirmation AND at least one idle job that's been
  // validated as a real disk image AND has a target picked.
  const ready = confirmed && jobs.some(j => j.state === 'idle' && j.target && j.validation === 'valid');
  const hasDone = jobs.some(j => j.state === 'success');
  const copiesValue = Math.max(1, Math.floor(Number(copies) || 1));
  const setCopies = (n) => onChangeCopies && onChangeCopies(Math.max(1, Math.floor(n)));
  return (
    <div className="toolbar">
      <div className="toolbar-left">
        <button className="btn btn-ghost" onClick={onAdd}>
          <span className="btn-bracket">[</span> {t('toolbar.add_image')} <span className="btn-bracket">]</span>
        </button>
        {onChangeCopies && (
          <div className="toolbar-copies mono small" aria-label={t('toolbar.copies_label')}>
            <span className="copies-label">{t('toolbar.copies_label')}</span>
            <button className="copies-step" onClick={() => setCopies(copiesValue - 1)} disabled={copiesValue <= 1} aria-label={t('toolbar.copies_dec')}>−</button>
            <input
              type="number"
              className="copies-input mono"
              min={1}
              value={copiesValue}
              onChange={(e) => setCopies(parseInt(e.target.value, 10) || 1)}
            />
            <button className="copies-step" onClick={() => setCopies(copiesValue + 1)} aria-label={t('toolbar.copies_inc')}>+</button>
          </div>
        )}
        {onAddFromUrl && (
          <button className="btn btn-ghost" onClick={onAddFromUrl}>
            <span className="btn-bracket">[</span> {t('toolbar.from_url')} <span className="btn-bracket">]</span>
          </button>
        )}
        {onBrowseCatalog && (
          <button className="btn btn-ghost" onClick={onBrowseCatalog}>
            <span className="btn-bracket">[</span> {t('toolbar.browse_catalog')} <span className="btn-bracket">]</span>
          </button>
        )}
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

/* ─────────── Entry Row ─────────── */

// EntryRow represents a queue ENTRY — an image with N copies still to be
// dispatched. Each dispatch spawns a JobRow below; the entry's counter
// decrements on each successful burn and the row disappears at 0.
function EntryRow({ entry, accent, onDispatch }) {
  const { t } = useTranslation();
  return (
    <div className="entry" style={{ '--accent': accent }}>
      <div className="entry-head">
        <div className="entry-icon mono">⧉</div>
        <div className="entry-image">
          <div className="entry-image-name">{entry.image.name}</div>
          <div className="entry-image-meta mono small">
            <span>{entry.image.size}</span>
          </div>
        </div>
        <div className="entry-counter mono">
          {t('entry.remaining', { remaining: entry.copiesRemaining, goal: entry.copiesGoal })}
        </div>
        <button
          className="btn btn-ghost entry-dispatch"
          onClick={onDispatch}
          disabled={entry.copiesRemaining <= 0}
        >
          <span className="btn-bracket">[</span> {t('entry.dispatch_next')} <span className="btn-bracket">]</span>
        </button>
      </div>
    </div>
  );
}

/* ─────────── Validation Badge ─────────── */

// Compact status pill for "does this image actually contain a burnable
// disk?". Driven by `job.validation` ('pending' | 'valid' | 'invalid');
// the optional `validationDetail` becomes a hover tooltip ("partition
// table: GPT (3 partitions)" / "compressed contents are not a
// recognised disk image" / etc.).
function ValidationBadge({ validation, detail }) {
  const { t } = useTranslation();
  if (validation === 'invalid') {
    return (
      <span className="vbadge vbadge--invalid" title={detail || t('job.validation.invalid')}>
        <span aria-hidden="true">⚠</span>
        <span className="vbadge-label">{t('job.validation.invalid')}</span>
      </span>
    );
  }
  if (validation === 'valid') {
    return (
      <span className="vbadge vbadge--valid" title={detail || t('job.validation.valid')}>
        <span aria-hidden="true">✓</span>
        <span className="vbadge-label">{t('job.validation.valid')}</span>
      </span>
    );
  }
  return (
    <span className="vbadge vbadge--pending" title={t('job.validation.pending')}>
      <span aria-hidden="true">…</span>
      <span className="vbadge-label">{t('job.validation.pending')}</span>
    </span>
  );
}

/* ─────────── Partition Strip ─────────── */

// gparted-style proportional bar. `partitions` is the PartitionSummary
// from the backend (or null when the image has no recognised layout,
// e.g. compressed sources or superfloppies). `totalBytes` is the
// uncompressed image size — used as denominator so the bar fills the
// whole row even when partitions don't cover the full disk (trailing
// unallocated space then shows up as a grey segment).
function PartitionStrip({ partitions, totalBytes, validation, validationDetail }) {
  const { t } = useTranslation();
  // Only show once we know the image is valid. The validation badge
  // already carries the pending / invalid state on its own row.
  if (validation !== 'valid') return null;
  // Valid but no partition table → superfloppy / unrecognised boot
  // sector. Render a single-cell bar tagged with the validation detail
  // ("filesystem: FAT32", "boot sector …") so the row still has
  // visual continuity with multi-partition images.
  if (!partitions || !partitions.partitions?.length) {
    return (
      <div className="pstrip pstrip--single">
        <div className="pstrip-bar">
          <div className="pstrip-seg pstrip-seg--unknown" style={{ width: '100%' }}>
            <span className="pstrip-seg-label mono">
              {validationDetail || t('job.partition.no_table')}
            </span>
          </div>
        </div>
      </div>
    );
  }
  const segments = buildPartitionSegments(partitions.partitions, totalBytes);
  const totalShown = segments.reduce((s, x) => s + x.length, 0);
  return (
    <div className="pstrip" aria-label={t('job.partition.strip.aria', { kind: partitions.table_kind })}>
      <div className="pstrip-bar">
        {segments.map((seg, i) => (
          <div
            key={i}
            className={`pstrip-seg pstrip-seg--${seg.fsClass}`}
            style={{ width: `${(seg.length / totalShown) * 100}%` }}
            title={`${seg.title}\n${seg.sizeHuman}`}
          >
            <span className="pstrip-seg-label mono">{seg.label}</span>
          </div>
        ))}
      </div>
      <div className="pstrip-meta mono small">
        <span>{partitions.table_kind}</span>
        <span className="dot">·</span>
        <span>{t('job.partition.count', { count: partitions.partitions.length })}</span>
      </div>
    </div>
  );
}

// Walks the partition list in start-order, inserting "unallocated"
// pseudo-segments wherever there's a gap before / between / after
// partitions. Without this, multi-partition images with a 1 MiB
// alignment hole at the front would render with their first partition
// hugging the left edge — fine, but the user wants to see "this disk
// has unused space" too.
function buildPartitionSegments(parts, totalBytes) {
  const sorted = parts.slice().sort((a, b) => a.start_bytes - b.start_bytes);
  const out = [];
  let cursor = 0;
  for (const p of sorted) {
    if (p.start_bytes > cursor) {
      const gap = p.start_bytes - cursor;
      out.push({
        fsClass: 'unallocated',
        length: gap,
        sizeHuman: formatBytes(gap),
        label: '',
        title: 'unallocated',
      });
    }
    out.push({
      fsClass: fsClassFor(p.filesystem),
      length: p.length_bytes,
      sizeHuman: p.size_human,
      label: p.label || p.filesystem || p.kind_label || '',
      title: [p.label, p.filesystem || p.kind_label].filter(Boolean).join(' · ') || p.kind_label,
    });
    cursor = p.start_bytes + p.length_bytes;
  }
  if (totalBytes && totalBytes > cursor) {
    const gap = totalBytes - cursor;
    out.push({
      fsClass: 'unallocated',
      length: gap,
      sizeHuman: formatBytes(gap),
      label: '',
      title: 'unallocated',
    });
  }
  return out;
}

// Map the backend's filesystem string into a CSS class suffix. Keeps
// the colour palette in CSS instead of inlined styles — switch the
// theme variable to retheme the strip globally.
function fsClassFor(fs) {
  const f = (fs || '').toLowerCase();
  if (!f) return 'unknown';
  if (f.startsWith('ext')) return 'ext';
  if (f.includes('exfat')) return 'exfat';
  if (f.includes('fat')) return 'fat';
  if (f.includes('ntfs')) return 'ntfs';
  if (f.includes('hfs')) return 'hfs';
  if (f.includes('apfs')) return 'apfs';
  if (f.includes('swap')) return 'swap';
  if (f.includes('iso')) return 'iso';
  if (f.includes('squash')) return 'squash';
  return 'unknown';
}

/* ─────────── Job Row ─────────── */

function JobRow({ job, accent, expanded, onToggle, onSelectTarget, onCancel, onRetry, onCopyHash, onCopyError, onFlashAnother, onRemove, density, fdaBlocked }) {
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
            <ValidationBadge validation={job.validation} detail={job.validationDetail} />
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

      <PartitionStrip
        partitions={job.partitions}
        totalBytes={job.image?.bytes}
        validation={job.validation}
        validationDetail={job.validationDetail}
      />

      {expanded && <JobDetail job={job} accent={accent} fdaBlocked={fdaBlocked} onCancel={onCancel} onRetry={onRetry} onCopyHash={onCopyHash} onCopyError={onCopyError} onFlashAnother={onFlashAnother} />}
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

function JobDetail({ job, accent, onCancel, onRetry, fdaBlocked }) {
  const { t } = useTranslation();
  const retryDisabled = !!job.retrying || (job.errorCode === 'ENEEDS_FDA' && fdaBlocked);
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

        <DetailBlock label={t('detail.block.partitions')} full>
          <PartitionTable
            partitions={job.partitions}
            validation={job.validation}
            validationDetail={job.validationDetail}
          />
        </DetailBlock>

        <DetailBlock label={t('detail.block.verification')} full>
          <VerificationPanel job={job} accent={accent} />
        </DetailBlock>
      </div>

      <div className="detail-actions">
        {job.state === 'writing' || job.state === 'verifying' ? (
          <button className="btn btn-danger" style={{ '--accent': accent, borderColor: accent, color: accent }} onClick={onCancel}>
            [ {t('detail.actions.abort')} ]
          </button>
        ) : null}
        {job.state === 'error' && (
          <>
            <button
              className={"btn btn-ghost" + (retryDisabled ? " is-disabled" : "")}
              onClick={retryDisabled ? null : onRetry}
              aria-disabled={retryDisabled || undefined}
            >
              [ {job.retrying
                  ? t('detail.actions.retrying', { defaultValue: 'RETRYING…' })
                  : (job.errorCode === 'ENEEDS_FDA' && fdaBlocked)
                    ? t('detail.actions.retry_waiting_fda', { defaultValue: 'WAITING FOR FDA' })
                    : t('detail.actions.retry')} ]
            </button>
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

/* ─────────── Partition Table (expanded detail) ─────────── */

function PartitionTable({ partitions, validation, validationDetail }) {
  const { t } = useTranslation();
  if (validation === 'pending') {
    return <div className="empty-line">{t('job.partition.pending')}</div>;
  }
  if (validation === 'invalid') {
    return <div className="empty-line">{validationDetail || t('job.validation.invalid')}</div>;
  }
  if (!partitions || !partitions.partitions?.length) {
    // Valid but no table — superfloppy / unrecognised. Surface the
    // validation detail string ("filesystem: FAT32", "boot sector …")
    // so the user knows _why_ there's no partition list.
    return (
      <div className="empty-line">
        {validationDetail || t('job.partition.no_table')}
      </div>
    );
  }
  return (
    <div className="ptable">
      <div className="ptable-head mono small">
        <span>{partitions.table_kind}</span>
        <span className="dot">·</span>
        <span>{t('job.partition.count', { count: partitions.partitions.length })}</span>
      </div>
      <table className="ver-table">
        <thead>
          <tr>
            <th>#</th>
            <th>{t('job.partition.col.start')}</th>
            <th>{t('job.partition.col.size')}</th>
            <th>{t('job.partition.col.kind')}</th>
            <th>{t('job.partition.col.fs')}</th>
            <th>{t('job.partition.col.label')}</th>
          </tr>
        </thead>
        <tbody>
          {partitions.partitions.map((p) => (
            <tr key={p.index}>
              <td className="mono">{p.index}</td>
              <td className="mono">{formatBytes(p.start_bytes)}</td>
              <td className="mono">{p.size_human}</td>
              <td>{p.kind_label}</td>
              <td>{p.filesystem || '—'}</td>
              <td>{p.label || '—'}</td>
            </tr>
          ))}
        </tbody>
      </table>
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
          {(() => {
            const decorated = disks.map((d) => {
              const tooSmall = jobImage && d.bytes < jobImage.bytes;
              const isInternal = d.bus.includes('NVME') || d.flags?.includes('INTERNAL');
              return { d, tooSmall, isInternal };
            });
            // Order: internal disks are "not permitted" regardless of size; among
            // non-internal disks, those that are too small for the image are split off.
            const allowed = decorated.filter((x) => !x.isInternal && !x.tooSmall);
            const tooSmall = decorated.filter((x) => !x.isInternal && x.tooSmall);
            const notPermitted = decorated.filter((x) => x.isInternal);
            const renderRow = ({ d, tooSmall, isInternal }) => (
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
            return (
              <>
                {allowed.length > 0 && (
                  <div className="disk-group-header mono small">{t('picker.allowed')}</div>
                )}
                {allowed.map(renderRow)}
                {tooSmall.length > 0 && (
                  <div className="disk-group-header mono small">{t('picker.too_small_header')}</div>
                )}
                {tooSmall.map(renderRow)}
                {notPermitted.length > 0 && (
                  <div className="disk-group-header mono small">{t('picker.not_permitted')}</div>
                )}
                {notPermitted.map(renderRow)}
              </>
            );
          })()}
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
  {
    key: 'catalog',
    fields: [
      { key: 'catalog.url', type: 'text', placeholder: 'https://diskcutter.app/catalog.json' },
      { key: 'catalog.refresh_hours', type: 'select',
        options: [
          { value: '0',   labelKey: 'prefs.catalog_refresh.off' },
          { value: '1',   labelKey: 'prefs.catalog_refresh.1h' },
          { value: '24',  labelKey: 'prefs.catalog_refresh.24h' },
          { value: '168', labelKey: 'prefs.catalog_refresh.7d' },
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
  'catalog.url': 'https://diskcutter.app/catalog.json',
  'catalog.refresh_hours': '24',
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
      <DoctorPanel />
    </div>
  );
}

// Stable id → i18n key. Unknown ids fall back to the backend's English
// `check.name`, which keeps the panel forward-compatible when new
// checks land before the locales catch up.
const DOCTOR_CHECK_LABELS = {
  'tmpdir': 'doctor.check.tmpdir',
  'tempdir-resolved': 'doctor.check.tempdir_resolved',
  'eject': 'doctor.check.eject',
  'fda': 'doctor.check.fda',
  'qemu': 'doctor.check.qemu',
};

function DoctorPill({ status }) {
  const { t } = useTranslation();
  const cls = 'doctor-pill doctor-pill--' + status;
  return <span className={cls + ' mono'}>[{t('doctor.status.' + status)}]</span>;
}

function DoctorPanel() {
  const { t } = useTranslation();
  const [report, setReport] = React.useState(null);
  const [loading, setLoading] = React.useState(true);
  const [error, setError] = React.useState(null);

  const run = React.useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await invoke('doctor');
      setReport(r);
    } catch (e) {
      setError(String(e));
      setReport(null);
    } finally {
      setLoading(false);
    }
  }, []);

  React.useEffect(() => { run(); }, [run]);

  return (
    <div className="detail-block prefs-block doctor-block">
      <div className="doctor-head">
        <div className="detail-label">▌ {t('doctor.title')}</div>
        <button className="btn btn-ghost" onClick={run} disabled={loading}>
          <span className="btn-bracket">[</span> R <span className="btn-bracket">]</span> {t('doctor.rerun')}
        </button>
      </div>
      {loading && !report && (
        <div className="doctor-status mono">{t('doctor.running')}</div>
      )}
      {error && (
        <div className="doctor-status doctor-status--err mono">{t('doctor.invoke_failed')}: {error}</div>
      )}
      {report && (
        <div className="doctor-rows">
          <div className="doctor-row doctor-row--overall">
            <span className="doctor-row-label mono">{t('doctor.overall')}</span>
            <DoctorPill status={report.overall} />
          </div>
          {report.checks.map((c) => {
            const labelKey = DOCTOR_CHECK_LABELS[c.id];
            const label = labelKey ? t(labelKey) : c.name;
            return (
              <div key={c.id} className="doctor-row">
                <DoctorPill status={c.status} />
                <span className="doctor-row-label">{label}</span>
                {c.note && <span className="doctor-row-note mono">— {c.note}</span>}
              </div>
            );
          })}
        </div>
      )}
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
  if (field.type === 'text') {
    return (
      <input
        type="text"
        className="prefs-text mono"
        value={value}
        placeholder={field.placeholder || ''}
        onChange={(e) => onChange(e.target.value)}
      />
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

// Field shape from `burn_history_list` (Rust → JSON, serde defaults so the
// keys arrive snake_cased): id, job_id, image_path, image_name, image_bytes,
// target_device, source_sha256, readback_sha256, verify_match, bytes_written,
// elapsed_ms, avg_write_bps, avg_verify_bps, state, error_code, error_message,
// started_at, finished_at.

function formatLogTimestampShort(ms) {
  if (!ms) return '—';
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function formatMMSS(ms) {
  if (ms == null) return '—';
  const total = Math.floor(ms / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

function shortDevice(device) {
  if (!device) return '—';
  return device.replace(/^\/dev\//, '');
}

function truncateMid(s, n) {
  if (!s) return '—';
  if (s.length <= n) return s;
  const half = Math.floor((n - 1) / 2);
  return `${s.slice(0, half)}…${s.slice(-half)}`;
}

function LogsView({ accent }) {
  const { t } = useTranslation();
  const [burns, setBurns] = React.useState([]);
  const [loading, setLoading] = React.useState(true);
  const [error, setError] = React.useState(null);
  const [filter, setFilter] = React.useState('all'); // 'all' | 'done' | 'failed'
  const [expanded, setExpanded] = React.useState({}); // { [id]: bool }

  const refresh = React.useCallback(async () => {
    setLoading(true);
    try {
      const rows = await invoke('burn_history_list', { limit: 500 });
      setBurns(Array.isArray(rows) ? rows : []);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  React.useEffect(() => { refresh(); }, [refresh]);

  // Re-load when a new burn lands so the view stays current without manual refresh.
  React.useEffect(() => {
    let mounted = true;
    const subs = [];
    const onActivity = () => { if (mounted) refresh(); };
    listen('disk-cutter://job-complete', onActivity).then((u) => subs.push(u));
    listen('disk-cutter://job-error', onActivity).then((u) => subs.push(u));
    return () => { mounted = false; subs.forEach((u) => u()); };
  }, [refresh]);

  // Rows arrive newest-first from the backend (ORDER BY started_at DESC).
  // Filter client-side per spec.
  const filtered = React.useMemo(() => {
    if (filter === 'done') return burns.filter((b) => b.state === 'success');
    if (filter === 'failed') return burns.filter((b) => b.state === 'error' || b.state === 'cancelled');
    return burns;
  }, [burns, filter]);

  const toggle = (id) => setExpanded((e) => ({ ...e, [id]: !e[id] }));

  return (
    <div className="logs">
      <div className="logs-bar">
        <div className="logs-filters" role="tablist">
          <LogsFilterBtn k="all"    label={t('logs.filter.all')}    active={filter === 'all'}    onClick={setFilter} />
          <LogsFilterBtn k="done"   label={t('logs.filter.done')}   active={filter === 'done'}   onClick={setFilter} />
          <LogsFilterBtn k="failed" label={t('logs.filter.failed')} active={filter === 'failed'} onClick={setFilter} />
        </div>
        <button className="btn btn-ghost logs-refresh" onClick={refresh}>
          <span className="btn-bracket">[</span> R <span className="btn-bracket">]</span> {t('logs.refresh_label')}
        </button>
      </div>

      {error && (
        <div className="logs-error mono small" style={{ background: accent, color: 'var(--on-accent)' }}>
          {error}
        </div>
      )}

      {loading ? (
        <div className="logs-loading mono small">{t('logs.loading')}</div>
      ) : burns.length === 0 ? (
        <LogsEmpty accent={accent} />
      ) : filtered.length === 0 ? (
        <div className="logs-loading mono small">{t('logs.no_match')}</div>
      ) : (
        <div className="logs-list">
          <div className="logs-head mono small">
            <span>{t('logs.col.when')}</span>
            <span>{t('logs.col.image')}</span>
            <span>{t('logs.col.target')}</span>
            <span>{t('logs.col.state')}</span>
            <span>{t('logs.col.duration')}</span>
            <span>{t('logs.col.throughput')}</span>
            <span />
          </div>
          {filtered.map((b) => (
            <LogsRow
              key={b.id}
              row={b}
              accent={accent}
              expanded={!!expanded[b.id]}
              onToggle={() => toggle(b.id)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function LogsFilterBtn({ k, label, active, onClick }) {
  return (
    <button
      type="button"
      className={"logs-filter" + (active ? " is-active" : "")}
      onClick={() => onClick(k)}
      role="tab"
      aria-selected={active}
    >
      {label}
    </button>
  );
}

function LogsRow({ row, accent, expanded, onToggle }) {
  const { t } = useTranslation();
  const isErr = row.state === 'error' || row.state === 'cancelled';
  return (
    <div className={"logs-row-wrap" + (expanded ? " is-open" : "") + (isErr ? " is-err" : "")}>
      <button className="logs-row" onClick={onToggle}>
        <span className="logs-when mono small">{formatLogTimestampShort(row.started_at)}</span>
        <span className="logs-image" title={row.image_name || row.image_path}>
          {truncateMid(row.image_name || row.image_path, 36)}
        </span>
        <span className="logs-target mono small">{shortDevice(row.target_device)}</span>
        <span className="logs-badge">
          <BurnStateBadge state={row.state} accent={accent} />
        </span>
        <span className="logs-dur mono small">{formatMMSS(row.elapsed_ms)}</span>
        <span className="logs-thru mono small">{formatBps(row.avg_write_bps)}</span>
        <span className="logs-chev">{expanded ? '▼' : '▶'}</span>
      </button>
      {expanded && <LogsRowDetail row={row} accent={accent} />}
    </div>
  );
}

function LogsRowDetail({ row, accent }) {
  const { t } = useTranslation();
  const mismatchCount = row.verify_match === false ? 1 : 0;
  return (
    <div className="logs-detail">
      <div className="detail-grid">
        <div className="detail-block">
          <div className="detail-label">▌ {t('logs.detail.image')}</div>
          <div className="detail-body">
            <KV k={t('logs.kv.path')} v={row.image_path || '—'} mono wrap />
            <KV k={t('logs.kv.name')} v={row.image_name || '—'} mono />
            <KV k={t('logs.kv.size')} v={formatBytes(row.image_bytes)} mono />
            <KV k={t('logs.kv.bytes_written')} v={row.bytes_written != null ? row.bytes_written.toLocaleString() : '—'} mono />
          </div>
        </div>

        <div className="detail-block">
          <div className="detail-label">▌ {t('logs.detail.target')}</div>
          <div className="detail-body">
            <KV k={t('logs.kv.device')} v={row.target_device || '—'} mono />
            <KV k={t('logs.kv.started')} v={formatLogTimestampShort(row.started_at)} mono />
            <KV k={t('logs.kv.finished')} v={row.finished_at ? formatLogTimestampShort(row.finished_at) : '—'} mono />
            <KV k={t('logs.kv.duration')} v={formatDuration(row.elapsed_ms || 0)} mono />
          </div>
        </div>

        <div className="detail-block detail-block--full">
          <div className="detail-label">▌ {t('logs.detail.verification')}</div>
          <div className="detail-body">
            <KV k={t('logs.kv.sha_source')} v={row.source_sha256 || '—'} mono wrap />
            <KV k={t('logs.kv.sha_readback')} v={row.readback_sha256 || '—'} mono wrap />
            <KV
              k={t('logs.kv.verify_match')}
              v={row.verify_match == null ? '—' : row.verify_match ? t('logs.match.ok') : t('logs.match.mismatch')}
              mono
            />
            <KV k={t('logs.kv.mismatches')} v={String(mismatchCount).padStart(3, '0')} mono />
            <KV k={t('logs.kv.avg_write')} v={formatBps(row.avg_write_bps)} mono />
            <KV k={t('logs.kv.avg_verify')} v={formatBps(row.avg_verify_bps)} mono />
          </div>
        </div>

        {(row.error_code || row.error_message) && (
          <div className="detail-block detail-block--full">
            <div className="detail-label" style={{ color: accent }}>▌ {t('logs.detail.error')}</div>
            <div className="detail-body">
              <KV k={t('logs.kv.error_code')} v={row.error_code || '—'} mono />
              <KV k={t('logs.kv.error_message')} v={row.error_message || '—'} mono wrap />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function LogsEmpty({ accent }) {
  const { t } = useTranslation();
  return (
    <div className="empty-state">
      <div className="empty-glyph" style={{ borderColor: 'var(--ink)' }}>
        <div className="empty-stripes" />
        <div className="empty-label mono">{t('logs.empty')}</div>
      </div>
      <div className="empty-help mono">
        <span style={{ color: accent }}>▶</span> {t('logs.empty_hint')}
      </div>
    </div>
  );
}

function BurnStateBadge({ state, accent }) {
  const { t } = useTranslation();
  const s = state || 'unknown';
  const isOk = s === 'success';
  const isErr = s === 'error' || s === 'cancelled';
  const label = isOk
    ? `✓ ${t('logs.state.success')}`
    : isErr
      ? `✕ ${t(`logs.state.${s}`, { defaultValue: t('logs.state.error') })}`
      : t(`logs.state.${s}`, { defaultValue: s.toUpperCase() });
  const style = isErr ? { background: accent, color: 'var(--on-accent)', borderColor: accent } : {};
  return (
    <span className={"log-state-badge log-state-badge--" + s} style={style}>{label}</span>
  );
}

/* ─────────── Catalog sheet ─────────── */

// Field shape from `catalog_list`:
//   { catalog: { schema_version, generated_at?, source_commit?, groups: [
//       { id, name, description?, images: [
//         { id, name, description, download_url, sha256sums_url, homepage,
//           size_bytes?, published_at?, arch? }
//       ] }
//     ] },
//     source: 'cached' | 'bundled' | 'remote',
//     loaded_at_ms, url }
//
// Fetched once when the sheet mounts. Click an entry → fires onPick(entry)
// which the parent wires to `start_download` from feat/url-fetch. If that
// command isn't registered (catalog branch merged before url-fetch), the
// invoke will reject and the parent should toast the failure.

function CatalogSheet({ open, onPick, onClose, accent }) {
  const { t } = useTranslation();
  const [response, setResponse] = React.useState(null);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState(null);
  const [refreshing, setRefreshing] = React.useState(false);

  React.useEffect(() => {
    if (!open) return;
    setLoading(true);
    setError(null);
    invoke('catalog_list')
      .then((r) => setResponse(r))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [open]);

  React.useEffect(() => {
    if (!open) return;
    const onKey = (e) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onClose]);

  const onRefresh = async () => {
    setRefreshing(true);
    setError(null);
    try {
      const r = await invoke('catalog_refresh');
      setResponse(r);
    } catch (e) {
      setError(String(e));
    } finally {
      setRefreshing(false);
    }
  };

  if (!open) return null;
  const groups = response?.catalog?.groups || [];
  const source = response?.source;

  return (
    <div className="sheet-backdrop" onClick={onClose}>
      <div className="sheet sheet--wide" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-head">
          <div>
            <div className="sheet-eyebrow">{t('catalog.eyebrow')}</div>
            <div className="sheet-title">{t('catalog.title')}</div>
          </div>
          <button className="sheet-x" onClick={onClose}>✕</button>
        </div>

        <div className="sheet-warning">
          <span style={{ color: accent }}>⚠</span>
          {t('catalog.warning')}
        </div>

        {error && (
          <div className="catalog-error mono small" style={{ background: accent }}>
            {error}
          </div>
        )}

        <div className="catalog-list">
          {loading ? (
            <div className="catalog-empty mono small">{t('catalog.loading')}</div>
          ) : groups.length === 0 ? (
            <div className="catalog-empty mono small">{t('catalog.empty')}</div>
          ) : (
            groups.map((g) => (
              <div key={g.id} className="catalog-group">
                <div className="catalog-group-head">
                  <div className="catalog-group-name">{g.name}</div>
                  {g.description && (
                    <div className="catalog-group-desc mono small">{g.description}</div>
                  )}
                </div>
                {g.images.map((img) => (
                  <button
                    key={img.id}
                    className="catalog-row"
                    onClick={() => onPick(img)}
                  >
                    <div className="catalog-row-body">
                      <div className="catalog-row-name">{img.name}</div>
                      <div className="catalog-row-desc mono small">{img.description}</div>
                      <div className="catalog-row-meta mono small">
                        {img.arch && <><span>{img.arch}</span><span className="dot">·</span></>}
                        {img.size_bytes != null && (
                          <><span>{formatBytes(img.size_bytes) || ''}</span><span className="dot">·</span></>
                        )}
                        {img.published_at && (
                          <><span>{img.published_at}</span><span className="dot">·</span></>
                        )}
                        <a
                          href={img.homepage}
                          target="_blank"
                          rel="noopener noreferrer"
                          onClick={(e) => e.stopPropagation()}
                        >{t('catalog.homepage')}</a>
                      </div>
                    </div>
                    <div className="catalog-row-pick">[ {t('catalog.pick')} ]</div>
                  </button>
                ))}
              </div>
            ))
          )}
        </div>

        <div className="sheet-foot mono small">
          <span>{t('catalog.source.' + (source || 'unknown'))}</span>
          {response?.url && (
            <>
              <span className="dot">·</span>
              <span title={response.url} className="catalog-foot-url">{response.url}</span>
            </>
          )}
          <span style={{ marginLeft: 'auto' }}>
            <button className="picker-link" onClick={onRefresh} disabled={refreshing}>
              {refreshing ? t('catalog.refreshing') : t('catalog.refresh')}
            </button>
            &nbsp;&nbsp;
            <button className="picker-link" onClick={onClose}>{t('picker.cancel')}</button>
          </span>
        </div>
      </div>
    </div>
  );
}

export {
  WindowChrome, Sidebar, DangerBanner, Toolbar,
  JobRow, EntryRow, DiskPickerSheet, LogsView,
  PrefsView, PREFS_DEFAULTS,
  CatalogSheet,
};
