export function formatBytes(n) {
  if (n == null) return '—';
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)} GB`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)} MB`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(0)} kB`;
  return `${n} B`;
}

// Compact human form for the queue row meta line: `31.98GB`. No
// space inside the unit because the row is tight on horizontal
// space and pairs the size with the validation chip — a tight glyph
// reads more like one token alongside the chip.
export function formatBytesCompact(n) {
  if (n == null) return '—';
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)}GB`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}MB`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(0)}kB`;
  return `${n}B`;
}

// "Human + exact" form for the expanded detail block: `31.98GB
// (31,000,000,000 bytes)`. The detail view has room for both, and the
// exact bytes matter when comparing image size to target capacity.
export function formatBytesExtended(n) {
  if (n == null) return '—';
  return `${formatBytesCompact(n)} (${n.toLocaleString()} bytes)`;
}

export function formatBps(bps) {
  if (bps == null) return '—';
  if (bps >= 1e9) return `${(bps / 1e9).toFixed(2)} GB/s`;
  if (bps >= 1e6) return `${(bps / 1e6).toFixed(1)} MB/s`;
  if (bps >= 1e3) return `${(bps / 1e3).toFixed(0)} kB/s`;
  return `${bps} B/s`;
}

export function formatDuration(ms) {
  const total = Math.floor((ms || 0) / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

export function formatSession(ms) {
  const total = Math.floor((ms || 0) / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  return `${h}h ${String(m).padStart(2, '0')}m`;
}

export function makeJob(num, image, target, parentEntryId) {
  return {
    id: `job-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    num,
    image,
    target,
    parentEntryId: parentEntryId || null,
    state: 'idle',
    progress: 0,
    verifyProgress: 0,
    speed: '—',
    eta: '—',
    elapsed: '—',
    errorCode: undefined,
    errorMessage: undefined,
    verification: null,
    validation: 'pending',
    validationDetail: null,
    // Partition summary lands once content validation succeeds: shape
    // is `{ table_kind: 'MBR'|'GPT', partitions: [PartInfo] }` or
    // `null` for compressed / superfloppy / unrecognised layouts. The
    // PartitionStrip component handles either case.
    partitions: null,
    // Image-level bootability — fires alongside partition probe once
    // validation passes. Distinct from per-partition `partition.bootable`
    // because some bootable images (ISO with El Torito only, MBR with
    // bootloader code but no active partition) carry no bootable
    // partition entry. Shape `{ bootable: bool, sources: BootSource[] }`
    // or `null` while still loading / on sources we can't probe.
    boot: null,
    // User-tunable write knobs in effect when this burn was dispatched
    // (writer impl, chunk size, worker count, etc.). The backend
    // snapshots them at start_write time and emits them on
    // `disk-cutter://burn-started`. `null` until the burn starts;
    // shape `{ "writer.impl": "pipelined", "chunk.bytes": "1048576", … }`.
    burnParams: null,
  };
}

export function makeEntry(image, copies) {
  const goal = Math.max(1, Math.floor(Number(copies) || 1));
  return {
    id: `entry-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    image,
    copiesGoal: goal,
    copiesRemaining: goal,
  };
}

export function decrementEntry(entry) {
  if (!entry) return entry;
  const next = Math.max(0, entry.copiesRemaining - 1);
  return { ...entry, copiesRemaining: next };
}
