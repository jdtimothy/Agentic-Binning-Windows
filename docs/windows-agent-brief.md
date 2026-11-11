# Brief for the first session on a Snapdragon X PC

For a coding agent working in this repository on the owner's Windows computer. `docs/testing-on-windows.md` is the same ground as a checklist for a person; this page says what to do, in what order, and what to write down.

## Where things stand

Everything was written and checked on Linux with a simulated model. The Build workflow compiles and tests on GitHub's `windows-11-arm` runner, so the code builds for Windows. These have never run anywhere:

- a real `geniex` command (the flags and output shape were read from the CLI's source, not observed),
- the app's window on WebView2,
- the window's own video frame reader (used when FFmpeg is absent),
- file moves on NTFS: another drive, a locked file, a long path.

Expect the first real `geniex infer` to need a fix. That is the purpose of this session.

## Rules

Read `CLAUDE.md` first. On top of it:

- **No paid Claude call.** Before adding media to any project, read `sortingModel` in `%LOCALAPPDATA%\Agentic Binning\settings.json`. If it is `Claude`, stop and ask the owner. Never set or read the API key.
- **Bin Files has no undo.** Run it only on a project made in this session from files you created or copied. Never on a folder of the owner's own photos. Record a SHA-256 of every file before and compare after.
- **GenieX stays a command.** If a flag is wrong, fix the flag in `crates/core/src/geniex.rs` and update the notes at the top of that file. Don't switch to the C API or a library.
- Work on the branch you were given. Small commits, one finding each.
- When a check fails, write down the exact command and its output before changing code.

## Step 0: tools

Check, and report what is missing instead of installing a different version of anything:

```powershell
rustc -vV            # host must be aarch64-pc-windows-msvc
cargo --version
node --version       # only for the Tauri CLI
tauri --version      # npm install -g @tauri-apps/cli@2
geniex --version
geniex list --format json
ffmpeg -version      # optional
```

The Rust build needs Visual Studio Build Tools with the ARM64 C++ tools. WebView2 ships with Windows 11.

## Step 1: the tests

```powershell
cargo test -p binning-core
```

All must pass. Three tests are Unix-only (`#[cfg(unix)]`) and don't run here; one of them is the test that drives a stand-in GenieX written in Python.

## Step 2: GenieX by hand

Run section 2 of `docs/testing-on-windows.md` exactly: `--version`, `list --format json`, then the one-photo `infer`. Save stdout and stderr of each to files. Check the five rows of that section's table and record a yes or no for each. Then also try:

- the same `infer` with `--grammar-path` pointing at a grammar the app wrote (the Activity log shows the path),
- a prompt file with several image path lines (how a video is sent),
- a path with a space in it,
- `infer` with the text model `LiquidAI/LFM3-1.7B-GGUF:Q4_0` and no image.

If any differs from what `geniex.rs` expects, fix `geniex.rs` and its parser tests before going on.

## Step 3: the core with real GenieX, in a browser

The dev server runs the real core without the window, which makes it easy to drive and to read errors:

```powershell
cargo run -p binning-devserver -- --data C:\temp\ab-data --projects C:\temp\ab-projects
```

Open `http://localhost:4173`. Settings → GenieX must show the version and both models. Make test media (the Android repository's `scripts/make-test-media.sh` under Git Bash with FFmpeg, or a dozen generated files), copy it somewhere disposable, then: New project → A folder → Create and analyze.

Expected with the Android test set: 7 added; `empty.mp4` and `still_truncated.jpg` end Failed; the rest analyzed, then categorized. Record seconds per photo and per clip, and tokens per second. Each item starts `geniex` again, so the model load time is paid every item: measure it, because it decides whether one command per item is acceptable.

For a failed item, Settings → Activity log has the full command. Run it in a terminal to see GenieX's own error.

## Step 4: the app itself

```powershell
cd src-tauri
tauri dev
```

Only the window has these, so check them here: the folder and file pickers, drag and drop of files onto the window, the title bar and resize, keep-awake during a run, and video frames without FFmpeg (take FFmpeg off the PATH for one H.264 `.mp4` and one HEVC `.mov`). Then build the installer with `tauri build` and install it once.

How it looks is the owner's call. Take screenshots of Analyze, Project Media and each sheet in light and dark, and leave them for the owner.

## Step 5: Bin Files

Section 6 of `docs/testing-on-windows.md`, on the test project only, with hashes before and after. Add the Windows cases: a source on another drive, a file held open by another program, an exFAT drive if one is at hand, a path over 260 characters, and two names that differ only in case.

## Step 6: tests worth adding

Add these as you go, in this order.

1. **Real GenieX tests**, marked `#[ignore]` so `cargo test` and CI skip them (run with `cargo test -p binning-core -- --ignored`). Model them on the Android device tests in `src/androidTest/.../model/`:
   - `GenieXVisionModelTest`: a photo on the NPU returns JSON that parses; a video's frames come back with timestamps; a missing file fails cleanly; cancelling a call leaves no `geniex` process behind.
   - `GenieXTextModelTest`: the grammar holds on ten items; ten items get sorted.
2. **File moves on Windows**, modelled on `MoveFilesDeviceTest`: a rename keeps size, time and content; a rename never replaces an existing file; same-drive is told from cross-drive; a copy's hash matches a read-back. Put them in `moves.rs` under `#[cfg(windows)]` so CI runs them.
3. **A Windows stand-in for GenieX**, so `drives_the_geniex_command_line` stops being Unix-only.
4. **Android unit tests for logic this app shares.** The Rust core has 58 tests; the Android app has about 60 test files for the same rules. Port by case list, not line by line, in this order: `MoveEngineTest`, `BinFilesJobTest`, `MovePlannerTest`, `FolderCheckTest`, `LayoutCheckTest` (file safety); `SortValidatorTest`, `CategorizerTest`, `ChunkPlannerTest`, `SortParserTest`; `AnalysisParserTest`, `VideoTimingTest`, `VisionBudgetTest`; `PipelineControllerTest`, `ResortJobTest`; `FtsQueryTest`, `CategoryNamesTest`, `MediaCountsTest`, `MediaStatusTest`; `ClaudeErrorMappingTest` (responses are canned: no network).

   Skip the view-model, Compose, DataStore, notification and Room migration tests: that code has no counterpart here.

If an Android case fails against the Rust code, the Android behaviour is the reference unless `CLAUDE.md` or a comment says the difference is deliberate.

## What to leave behind

`docs/notes/windows-first-run.md` with: tool versions; each step's result; every `geniex` command tried with its output; timings; what was fixed (commit) and what is still open. Update the status paragraph in `README.md` once a real run has worked.
