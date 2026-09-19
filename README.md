# Metteur

A Visualization-First Agent Based on the Plan-and-Execute Paradigm.

## Running the tests

```powershell
powershell -File scripts/test.ps1          # cargo test -j 1 --workspace
powershell -File scripts/test.ps1 -p metteur-daemon
```

`cargo test` needs the MSVC environment on `PATH` (otherwise the linker resolves to
Git for Windows' GNU `link.exe` and linking fails) and the daemon's test binary is
large enough that parallel linking is unsafe, so the script always passes `-j 1`.
The web client has its own suites: `pnpm typecheck`, `pnpm lint`, `pnpm test:e2e`,
`pnpm test:monkey` in `modules/webcore`.
