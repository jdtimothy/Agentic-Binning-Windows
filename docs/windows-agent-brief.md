# Brief for the first session on a Snapdragon X PC

For a coding agent working in this repository on the owner's Windows computer. `docs/testing-on-windows.md` covers the same ground as a checklist for a person. This page says what to run, in what order, what each result means, and what to write down.

## Where things stand

Everything was written and checked on Linux with a simulated model. The Build workflow compiles the app and runs the automated tests on GitHub's `windows-11-arm` machine, so the code builds on Windows and the file-move rules hold on NTFS. These have never run anywhere:

- a real `geniex` command (the flags and the output's shape were read from the CLI's source, not observed),
- the app's window on WebView2,
- the window's own video frame reader (used when FFmpeg is absent),
- file moves between two real drives, and on exFAT.

Expect the first real `geniex infer` to need a fix. Finding that fix is the purpose of this session.

## Rules

Read `CLAUDE.md` first. On top of it:

- **No paid Claude call.** The tests below never call Claude. Before adding media to a project in the app itself, read `sortingModel` in `%LOCALAPPDATA%\Agentic Binning\settings.json`. If it is `Claude`, stop and ask the owner. Never set or read the API key.
- **Bin Files has no undo.** The tests make their own files in temporary folders. By hand, run Bin Files only on a project made in this session from files you created or copied, never on a folder of the owner's photos.
- **GenieX stays a command.** If a flag or the output's shape is wrong, fix it in `crates/core/src/geniex.rs`, update the notes at the top of that file, and add the real output to its parser tests. Don't switch to the C API or a library.
- **Evidence before a fix.** When a test fails, save its full output first (it prints the exact command), then run that command in a terminal and save what GenieX prints. Fix the code only after that.
- Work on the branch you were given. Small commits, one finding each.

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

## Step 1: the automated tests

```powershell
cargo test -p binning-core
```

About 430 tests; all must pass. They use no GenieX and no network. They include the Android app's own test cases (files named `*_android_cases.rs`) and tests only Windows can run (`windows_tests` in `files.rs`, `on_windows` in `moves_android_cases.rs`): a file another program holds, paths over 260 characters, names that differ only in case, reserved names. The same run is green on GitHub's Windows machine, so a failure here is about this PC: report it with its output before going on.

## Step 2: GenieX

These tests are in `crates/core/tests/on_device.rs`. They are skipped by default. Run them one at a time, in order, and keep each one's output:

```powershell
cargo test -p binning-core --test on_device g01 -- --ignored --nocapture --test-threads=1 2>&1 | Tee-Object g01.txt
```

Replace `g01` with each name's first three characters. `--nocapture` prints the reply, its numbers and the exact command. `--test-threads=1` keeps two commands from running on the NPU at once. If `geniex.exe` is not on the PATH, set `$env:AB_GENIEX` to its full path first. `$env:AB_COMPUTE = "cpu"` (or `gpu`) reruns anything off the NPU, which tells an NPU driver problem from a problem in the command.

| Test | What it proves | If it fails |
| --- | --- | --- |
| `g01` | `geniex --version` and `geniex list --format json` answer in the shape the app reads, and both models are downloaded. | A missing model: run the `geniex pull` line the failure prints. Models listed but not recognised: the JSON changed; fix `parse_list` and `has_model`. Do this before anything else: every other test needs it. |
| `g02` | One photo. The command ends by itself, the picture is attached (the reply says "red"), the reply is the app's JSON, it ends with `eos` within 400 tokens. | Hangs: `-i` no longer ends the session. No "red": the image path was not found in the prompt file; run `g05`, then try the path with backslashes. Not JSON: run `g05`. No stop reason: `--verbose` or `--test-mode` output changed; fix `parse_test_mode` or `parse_stdout`. |
| `g03` | The same with spaces in every path, as under `C:\Users\First Last`. | The CLI's image-path pattern stops at a space. The fix is to keep work files under a path without spaces (the app's `work` folder); note what worked. |
| `g04` | Eight frames sent as one clip: 2 to 5 actions with `m:ss` times. A note is printed if the reply misses the colour change half way. | No actions or bad times: the grammar is not applied. The note alone means the model may not be given all frames; record it, it is not a failure. |
| `g05` | A photo without `--grammar-path`. | It passes when `g02` fails: the grammar flag or file is the problem, not the image. Both fail: the image or the model. |
| `g06` | Stop ends the call within 10 seconds and the next call works. With `AB_NO_SERVE=1` it also checks that no `geniex` process is left. | A process left behind: the CLI starts a helper that outlives it; `GenieCli::cancel` must end the whole process tree. |
| `g07` | No pass or fail. Prints seconds per photo. | Write the numbers down. The app keeps one `geniex serve` running, so the model is loaded once. Run it again with `$env:AB_NO_SERVE = "1"` to see one `geniex infer` per photo, which is what the app falls back to. |
| `g08` | The sorting model sorts ten items in one call; the grammar makes the reply list each id once. | Wrong ids: `--grammar-path` is not honoured for the text model. A printed "would ask again" note is the small model's judgement, not a fault. |
| `g09` | A grammar holding existing category names with spaces, an apostrophe and quotes is accepted. | GenieX rejects the grammar: read the grammar file beside the prompt file named in the command; fix `literal_of` in `sorting.rs`. |
| `g10` | The whole app with real models in its own temporary folders: add seven files, analyze, sort, Bin Files, then compare every file's SHA-256. | It prints the activity log and each item's result. A file missing or changed after Bin Files is the most serious finding possible: stop and report. |

After `g10` passes, the app's core works on this PC. What is left needs the window (Step 4) or a person's eyes.

## Step 3: file moves

Most file-move rules are already covered by Step 1. Three things need this PC.

**Between two drives.** Point the tests at an empty folder on another drive. They make their own folder inside it and remove it afterwards:

```powershell
$env:AB_OTHER_DRIVE = "D:\ab-test"
cargo test -p binning-core --test on_device m0 -- --ignored --nocapture --test-threads=1 2>&1 | Tee-Object moves.txt
```

| Test | What it proves |
| --- | --- |
| `m01` | Files on the other drive, project on this one. Each file is copied, read back and compared, put in place, and only then is the original deleted. Hashes and times match afterwards; no half copy is left. |
| `m02` | The other way round: the project on the other drive. |
| `m03` | A move inside the other drive never replaces a file. |

Run all three once with an internal or USB NTFS drive, and once more with an **exFAT** USB stick if there is one: exFAT has no hard links and coarser file times. If `m01` fails only on exFAT because the file's time differs by a second or two, record the two times; that is a finding about exFAT, not data loss.

Without a second drive these print `SKIPPED`. Say so in the notes; do not count them as passed.

**A run interrupted for real.** The tests simulate a crash by editing the database. Do it once for real, in the dev server (Step 4) on a test project of about 200 copied files: press Bin Files, then end the process from Task Manager while files move. Start it again: the run must resume by itself and finish, and the report must count every file. Compare hashes before and after as `g10` does (`Get-FileHash -Algorithm SHA256`).

**Things only a person can cause.** Once each, on test files: a file open in a video player during Bin Files (skipped, reported, untouched, moved by the next Bin Files); a OneDrive folder as the source (files that are online-only may fail to copy: record what the report says); the PC put to sleep during a run.

## Step 4: the app in a browser, then its window

The dev server runs the real core without the window, which is easy to drive and to read errors from:

```powershell
cargo run -p binning-devserver -- --data C:\temp\ab-data --projects C:\temp\ab-projects
```

Open `http://localhost:4173`. Settings → GenieX must show the version and both models. Make test media (the Android repository's `scripts/make-test-media.sh` under Git Bash with FFmpeg, or a dozen generated files), copy it somewhere disposable, then: New project → A folder → Create and analyze. Expected with the Android test set: 7 added; `empty.mp4` and `still_truncated.jpg` end Failed; the rest analyzed, then categorized. For a failed item, Settings → Activity log has the full command.

Then the window itself:

```powershell
cd src-tauri
tauri dev
```

Only the window has these, so check them here: the folder and file pickers, dragging files onto the window, the title bar and resizing, keep-awake during a run, and video frames without FFmpeg (take FFmpeg off the PATH and try one H.264 `.mp4` and one HEVC `.mov`). Then build the installer with `tauri build` and install it once.

How it looks is the owner's call. Take screenshots of Analyze, Project Media and each sheet in light and dark, and leave them for the owner.

## Step 5: tests still worth adding

- Once a real `geniex infer` has run, paste its real stdout and stderr into the parser tests in `geniex.rs`, replacing the samples written from the CLI's source.
- A Windows stand-in for GenieX, so `drives_the_geniex_command_line` in `tests/end_to_end.rs` stops being Unix-only.
- The Android app's unit tests for logic this app shares are all ported (files named `*_android_cases.rs`, and sections marked "The Android app's ..." in the smaller modules). Not ported, because that code has no counterpart here: the view-model, Compose, notification, Android Keystore and Room migration tests.
- The Claude API key store is the one piece with no test on Windows: there it is the signed-in user's real Credential Manager, which a test must never write to. Check it once by hand in Settings only if the owner asks.

If an Android case fails against the Rust code, the Android behaviour is the reference unless `CLAUDE.md` or a comment says the difference is deliberate.

## What to leave behind

`docs/notes/windows-first-run.md` with:

- tool versions, the PC's model and chip, and the GenieX and NPU driver versions;
- one line per test `g01` to `g10` and `m01` to `m03`: passed, failed, or skipped and why;
- the saved output of every test that failed or printed a NOTE, and of every `geniex` command run by hand;
- the numbers from `g07`;
- what was fixed (with the commit) and what is still open.

Update the status paragraph in `README.md` once `g10` has passed.
