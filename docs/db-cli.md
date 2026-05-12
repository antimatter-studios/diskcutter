# `scripts/db` — SQLite inspector for the Disk Cutter database

A thin Bash wrapper around the `sqlite3` CLI that fronts the queries you'd
otherwise type by hand. Useful when debugging a failed burn, sanity-checking
that migrations applied, or pulling rows out for a ticket.

## Platform support

**macOS only.** The script exits with code `2` on Linux/Windows. It assumes:

- `sqlite3` is on `$PATH` (ships with macOS — `xcode-select --install` if not)
- The app has been launched at least once so the database exists at
  `~/Library/Application Support/com.diskcutter.app/disk-cutter.sqlite`

To point at a different DB file (a backup, a test fixture):
```sh
DISKCUTTER_DB=/tmp/test.sqlite scripts/db tables
```

## Quick reference

```sh
scripts/db                          # prints usage
scripts/db path                     # absolute path to the live DB
scripts/db tables                   # all tables + row counts
scripts/db schema [TABLE]           # CREATE statements (all or one)
scripts/db migrations               # which migrations have been applied
scripts/db config                   # k/v config dump
scripts/db list_burn_history [N]    # N most recent burns (default 50)
scripts/db list_logs [BURN_ID]      # event log — filtered by burn or whole table
scripts/db query "<SQL>"            # arbitrary SQL — use with care
scripts/db vacuum                   # VACUUM to reclaim space
```

## What each command does

### `path`
Prints the resolved DB path. Handy as `cat "$(scripts/db path)"`-style
plumbing, or for opening in another tool:
```sh
open "$(scripts/db path | xargs dirname)"
```

### `tables`
Builds a single `UNION ALL` query so the row counts come back in one
sqlite3 invocation rather than N. Hides `sqlite_*` internals.

### `schema [table]`
Plain passthrough to `.schema`. Supply a table name to narrow it. Useful when
investigating "did my migration actually add that index?":
```sh
scripts/db schema burn_history
```

### `migrations`
Lists rows from `schema_migrations`, formatting `applied_at` (stored as
millis since epoch) as local time. If this comes back empty but the schema
tables exist, you're looking at a database created by an older binary
**before** the migration system was wired up — wipe it and let the app
recreate it cleanly:
```sh
rm "$(scripts/db path)"*    # nukes .sqlite, .sqlite-shm, .sqlite-wal
```

### `config`
Dumps the `config` table — currently used for things like the user's
selected `language`. Each row is one key/value pair.

### `list_burn_history [LIMIT]`
Most recent burns, newest first. Default limit is 50. Columns: `id`,
`started` (local time), `state` (`running`/`success`/`error`/`cancelled`),
`image_name`, `target_device`, `written` (GB), `ms` (elapsed),
`err` (error code if any).

The `id` column is the burn ID you feed to `list_logs`.

### `list_logs [burn_id]`
The event stream for burns — start, completion, errors. Ordered by
timestamp ascending. Pass a burn id to scope to one burn; omit it to dump
the whole `burn_logs` table (the `burn_id` column is included in that case
so you can grep). Use this to dig into *why* a specific burn ended the way
it did.

```sh
# find the most recent failed burn and look at its logs
scripts/db query "SELECT id FROM burn_history WHERE state='error' ORDER BY started_at DESC LIMIT 1"
scripts/db list_logs 42
```

### `query "<SQL>"`
Runs any SQL through the formatted output. Quote the whole statement:
```sh
scripts/db query "SELECT COUNT(*) FROM burn_history WHERE state='success'"
scripts/db query "DELETE FROM burn_history WHERE state='error'"   # destructive — careful
```

There are no safety rails. Treat this as `sqlite3` with prettier output.

### `vacuum`
Runs `VACUUM`. Reclaims space after a lot of `DELETE`s and defragments the
file. Cheap; safe to run any time the app isn't writing.

## Output format

The script uses `sqlite3 -mode box` (unicode-bordered table) when the
installed sqlite3 supports it (≥ 3.33, true on every supported macOS
release). Older sqlite versions fall back to `-mode column`.

Empty queries print nothing — that's sqlite3's behaviour in box mode, not a
script error. `echo $?` to confirm success/failure.

## When to reach for this vs. the app

The app itself doesn't yet surface burn history in the UI; the SQLite layer
records every burn but the React side only renders the in-memory job list.
Until that UI lands, `scripts/db list_burn_history` / `list_logs` is the
only way to see past burns.
