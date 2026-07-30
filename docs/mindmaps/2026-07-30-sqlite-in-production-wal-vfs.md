# Mindmap: SQLite in Production (WAL, Concurrency, VFS)

Source: [Micrologics article](https://micrologics.org/blog/sqlite-in-production-optimizing-wal-mode-concurrency-and-vfs-layers-for-low-latency-app-servers) (2026-07-17)

Open the interactive view: [2026-07-30-sqlite-in-production-wal-vfs.html](./2026-07-30-sqlite-in-production-wal-vfs.html)

```mermaid
mindmap
  root((SQLite in Production<br/>Low-Latency App Servers))
    Myth Busting
      Historically local-only
      Network RTT is bottleneck
      In-process on NVMe
      Sub-ms reads via mmap
      Defaults = safety not throughput
    WAL Mode
      vs Rollback Journal
        Writes block reads
        Single access on write
      Enable
        PRAGMA journal_mode=WAL
      Benefits
        Concurrent R+W
        Writers append to .sqlite-wal
      Checkpointing
        PASSIVE
        FULL
        RESTART
        TRUNCATE
        Explicit bg checkpoint
        wal_checkpoint PASSIVE
      synchronous NORMAL
        Sync at critical points
        Safe integrity in WAL
    Concurrency SQLITE_BUSY
      Single writer still
      busy_timeout 5000
        Retry + exp backoff
      Transaction modes
        DEFERRED default
          Deadlock risk
        IMMEDIATE
          Reserve lock early
          Prefer for writes
        EXCLUSIVE
          Blocks all
      BEGIN IMMEDIATE
    Memory and Cache
      cache_size -64000
        ~64MB working set
      mmap_size
        Kernel page cache
        Up to multi-GB map
        Pointer reads
    Custom VFS
      Abstract FS ops
      Litestream
        Stream WAL to S3
        PITR near-zero cost
      LiteFS
        FUSE VFS cluster
        Read replicas
      Ephemeral cloud disk
        VFS replication mandatory
    Production Blueprint
      journal_mode WAL
      synchronous NORMAL
      busy_timeout 5000
      cache_size -64000
      mmap_size 1GB
      foreign_keys ON
      journal_size_limit 64MB
      auto_vacuum INCREMENTAL
    When to Use
      Yes
        Read-heavy
        Hundreds concurrent
        Few hundred GB
        Ultra-low latency
      No / Prefer Postgres
        Multi-region writes
        Multi-TB datasets
```

## Key takeaways

1. **In-process on fast local storage** removes network RTT — the real bottleneck for many app servers.
2. **WAL mode** unlocks concurrent reads/writes; manage **checkpoints** explicitly under load.
3. Still **one writer** → `busy_timeout` + `BEGIN IMMEDIATE` for write transactions.
4. **Cache + mmap** keep the working set off the slow path.
5. **VFS layer** (Litestream / LiteFS) for HA and cloud durability.
6. Prefer SQLite for **read-heavy, sub-TB, latency-sensitive** apps; Postgres for multi-region writes / multi-TB.
