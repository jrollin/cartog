# How to Set Up S3 Index Sync

> For the full `[remote]` config key reference, see [../reference/config.md](../reference/config.md).

## `cartog push [--remote <s3-url>]`

Upload the local index DB to S3-compatible storage (AWS S3, MinIO, Cloudflare R2,
floci). Built in by default (`remote-s3` feature, on); cartog still runs 100%
local until you configure `[remote]` or pass `--remote`.

```bash
cartog push                                       # uses [remote].url
cartog push --remote s3://team-bucket/main.sqlite # explicit override
```

What it does, in order:

1. Refuses to push while `cartog serve` or `cartog watch` is using the DB.
2. Runs `PRAGMA wal_checkpoint(TRUNCATE)` so the file is self-contained.
3. Streams a SHA-256 hash of the DB.
4. Uploads via multipart with object metadata: `x-amz-meta-sha256`,
   `x-amz-meta-schema-version`, `x-amz-meta-cartog-version`, and
   `x-amz-meta-git-commit` (the commit the index was built at; omitted when
   the index has no git provenance). The `--json` output adds a `git_commit`
   field (`null` when absent) so a puller can decide whether the remote index
   matches its checkout.

Credentials come from the AWS environment chain (env vars, `~/.aws/credentials`,
IMDS) — **never from `.cartog.toml`**. Storing a credential-shaped key
(`access_key`, `secret_key`, `aws_*`, etc.) in `[remote]` fails at config-load
time with a security error.

## `cartog pull [--remote <s3-url>] [--force] [--no-sign-request]`

Download a prebuilt index from S3-compatible storage. Useful for CI warm-start
and for sharing a team-wide index instead of every dev rebuilding from zero.

```bash
cartog pull                              # uses [remote].url
cartog pull --remote s3://b/k.sqlite     # explicit override
cartog pull --force                      # overwrite even while peer is using the DB
cartog pull --no-sign-request            # anonymous (public-bucket) pull
```

Safety guarantees:

- **Atomic install** — the file is downloaded to `<db>.partial`, verified,
  then renamed; a mid-pull crash or network failure never leaves a torn DB.
- **Checksum required** — refuses to install if the remote object has no
  `x-amz-meta-sha256` metadata. Same for `x-amz-meta-schema-version`.
- **Non-cartog files refused** — pulling a SQLite file that lacks cartog's
  schema (e.g. an unrelated app's DB) is refused even when its sha256
  matches; cartog cross-checks the `schema_version` row against the header.
- **Commit provenance (report-only)** — pull prints the commit the index was
  built at (`commit=<short>`, also `git_commit` in `--json`). When both the
  `x-amz-meta-git-commit` header and the file's `last_commit` row are present,
  a mismatch is reported but never blocks installation. The install always
  proceeds; the caller (CI script, agent) decides whether the reported commit
  is fresh enough.
- **Schema-version guard** — refuses to install a DB produced by a newer
  cartog, naming both the pulled and supported versions.
- **WAL/SHM cleanup** — stale `db-wal` / `db-shm` siblings are deleted
  before rename to prevent SQLite from replaying phantom WAL frames.
- **Peer-process exclusion** — pull holds the same PID locks that
  `cartog serve` and `cartog watch` contend for, for the entire
  download → verify → install sequence. A running peer makes pull refuse
  up front; a peer (or second pull) starting mid-pull loses its lock
  election instead of opening the file about to be swapped (SQLite holds
  the file by inode; a rename under a live handle would corrupt its
  view). A `cartog serve` that starts during a pull attaches read-only
  and promotes onto the freshly pulled DB once the pull releases the
  lock. `--force` still takes the locks when no peer is live; only when
  a peer already holds a slot (or the lock dir is unusable) does it
  proceed unguarded, accepting that corruption risk for the live peer.

> **Trust boundary**: the `x-amz-meta-sha256` header is self-attested by
> whoever pushed the object — it catches corruption and accidental swaps
> but not a deliberate malicious push by someone with write access to the
> bucket. Treat the bucket like a shared filesystem under the same
> trust assumptions as your team's git remote.

## Configuring `[remote]`

In `.cartog.toml`:

```toml
[remote]
url        = "s3://team-bucket/cartog/main.sqlite"
region     = "us-east-1"
endpoint   = "https://minio.example.com"   # only for MinIO / R2 / floci
path_style = true                          # required for most non-AWS endpoints
```

Only those four keys are accepted. Credential-shaped keys (`access_key`,
`secret_key`, `aws_*`, `token`, `password`, …) are rejected at parse time —
configure credentials via the AWS environment chain instead.
