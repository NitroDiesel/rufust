# Contributing

## Development

```sh
cargo test --workspace
cargo build
cargo run -p rufust
```

The desktop has no environment-variable bypass for destructive writes. On
Linux, a live operation is available only through udisks2 or the installed
polkit policy and `/usr/libexec/rufust-helper`; on Windows, only through the
UAC-elevated worker. Use the helper's `--dry-run` mode for request validation,
disposable loop devices for Linux integration tests, and
`scripts/windows/vhd-tests.ps1` (elevated) for throwaway VHDX disks on
Windows. Never test on a disk containing useful data.

## Safety rules

- Keep raw-disk I/O out of the desktop process, and never run the window as
  root or administrator.
- Revalidate device identity in the helper before the first write.
- Prefer fixed absolute tool paths; never shell out with user-controlled commands.
- Document unavailable Windows-only features honestly (see `docs/CAPABILITIES.md`).

## Style

- Neat, non-redundant modules; one code path per concern.
- Unit tests for planning, safety, image sniffing, and protocol round-trips.
