# Agentic Binning for Windows

Windows (Snapdragon X Elite / X2 Elite) version of Agentic Binning for Android
(`jdtimothy/Agentic-Binning-Android`). Same product rules; when a rule is
unclear, the Android source is the reference, and the Rust module names its
Kotlin original in a comment.

A first session on a real Snapdragon X PC starts with `docs/windows-agent-brief.md`.

## Layout

- `crates/core`: all logic, no UI. `app.rs` is the command interface
  (`App::dispatch`) the window calls; `pipeline.rs` runs one job at a time.
- `src-tauri`: the Tauri 2 shell. Only window concerns live here.
- `ui`: plain HTML, CSS and ES modules. No bundler, no npm install.
- `crates/devserver`: serves `ui` to a browser with the real core, for work
  without Windows.

## Checks

```bash
cargo test -p binning-core                 # unit and end-to-end tests (simulated model)
cargo build -p agentic-binning             # the shell; on Linux needs libwebkit2gtk-4.1-dev
cargo run -p binning-devserver -- --data /tmp/ab-data --projects /tmp/ab-projects
```

The Windows build itself is checked by the Build workflow on GitHub's
`windows-11-arm` runner: a cloud session can't download the Windows SDK.

## Rules that are easy to break

- **GenieX is only ever run as a command** (`crates/core/src/geniex.rs`). The
  owner asked for terminal commands called by the app, not the C API. The
  notes at the top of that file say what was read from the CLI's source; read
  them before changing a flag or the prompt file's shape.
- No line of the vision prompt except the image path lines may contain `/` or
  `\`: the CLI finds image paths by pattern and would take the rest of the line.
- Paths are stored and compared with forward slashes (`paths::norm`), compared
  ignoring case. Don't use `std::path` joins for stored paths.
- The move engine never overwrites and never deletes an unverified original.
  Any change there needs a test in `moves.rs`.
- Never make a paid Claude call in a test or a check.
- The simulated model's output must always say it is simulated.
- Bin Files has no undo: test moves only on files the test created.
- The database schema is the Android app's version 5. A change needs a
  migration in `db.rs` and a decision about the Android app.
