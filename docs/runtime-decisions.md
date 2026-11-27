# Runtime decisions

## Direct TCP authentication (2026-11-27)

The daemon validates a nonblank token before opening a direct TCP RPC listener, including
loopback. Windows-to-WSL uses the same policy. Local Unix sockets and Windows named pipes can
rely on local access controls. The external status hook has a separate protocol and accepts
only local IPC. Remote clients can forward to the daemon Unix socket over SSH.

[The README](../README.md#run-the-daemon-on-another-machine) describes credential configuration.
The daemon integration fixture checks rejected configurations and independent authenticated
clients. The installed Windows smoke exercises the authenticated transport and workspace
reconnection. A direct TCP token authenticates connections. Use SSH when the transport
itself must be protected on an untrusted network.

## Unreadable workspace snapshots (2026-11-27)

The daemon preserves the original bytes of a rejected startup snapshot. Workspace creation
and closure remain available in memory. The workbench displays a connection-level warning,
and the CLI writes a best-effort warning to stderr after a successful mutation. Project setup
saves that would replace the rejected snapshot still fail. Pong carries optional persistence
health so older clients and daemons can continue connecting.

Daemon and CLI integration tests check the original bytes and success semantics. UI tests
check the warning and its removal on a healthy reconnect.

## Managed workbench cache (2026-11-27)

Persisted workbench documents are cached while their workspace is open. The store caches raw
documents and overlays the latest project setup on each returned copy. Disk-backed entries
have a 128-entry limit. Close retires scheduled readers before clearing the cached entry.
Update a live document through a daemon API. Use the shared checkout document for repo-based
configuration. Reopening or restarting reads the managed document from disk again.

Store and daemon fixtures cover cold reads, revision precedence, failed writes, and restart.
The Linux quality gate also checks that viable mutants in the selected read and revision
logic are caught.
