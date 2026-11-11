# First run on a Snapdragon X PC

11 November 2026, following `docs/windows-agent-brief.md`. Saved outputs are in `output/`, screenshots in `screens/`.

## Result

The app's core works on this PC with real GenieX. Every automated and on-device test that could run here passed. Two bugs were found and fixed. Not done: file moves between two drives and on exFAT (this PC has one drive), and the checks that need the app's own window driven by hand.

## The PC and tools

| | |
| --- | --- |
| PC | ASUS Zenbook A16 UX3607OA, Snapdragon X2 Elite Extreme X2E96100, 48 GB, Windows 11 Pro build 28000 |
| GenieX | CLI v0.10.0, QAIRT runtime 2.50, LlamaCPP runtime 19cce69 |
| NPU driver | Qualcomm Hexagon NPU 30.0.228.10000 (2026-07-20) |
| GPU driver | Qualcomm Adreno X2-90 32.0.172.2 (2026-09-08) |
| Rust | rustc and cargo 1.97.1, host `aarch64-pc-windows-msvc` |
| Node | 26.5.1 |
| Tauri CLI | 2.12.4. It was missing; installed with `npm install -g @tauri-apps/cli@2` as the brief says |
| FFmpeg | 8.1.1 essentials (gyan.dev) |
| Visual Studio | Build Tools 2026 (18.9.1) and 2022 (17.14.41), both with ARM64 C++ tools; Windows SDK 10.0.26100 |
| Models | LiquidAI/LFM3-VL-2B-GGUF Q4_0 and LiquidAI/LFM3-1.7B-GGUF Q4_0, both downloaded |

**Build needs the Visual Studio environment on this PC.** In a plain PowerShell, `cargo test` fails building `ring`: `fatal error: 'assert.h' file not found`. The `clang` on the PATH is LLVM-MinGW (installed by winget), and it cannot find the Windows headers by itself. Running `vcvarsall.bat arm64` first (or using "Developer PowerShell for VS") fixes it. Nothing in the repository is wrong; GitHub's machine has no MinGW clang in the way.

## Tests

| Test | Result |
| --- | --- |
| Step 1, `cargo test -p binning-core` | Passed: 434 + 3 at the start, 436 + 4 after this session's additions |
| `g01` | Passed. Version and both models read correctly |
| `g02` | Passed first time. 18.3 s, `eos`, 58 tokens, 66 tokens a second |
| `g03` | Passed. Paths with spaces work |
| `g04` | Passed, with the NOTE: the reply describes only a red square and gives `0:00` for both actions, so the model may be seeing only some of the 8 frames (`output/g04.txt`) |
| `g05` | Passed. The reply is the app's JSON even without the grammar |
| `g06` | Passed. Stopped 1.0 s after asking, no process left, next call worked |
| `g07` | Numbers below |
| `g08` | Passed, with the NOTE: the first reply used tag lists such as `turtle,beach,sand` as category names, so the app would ask again (`output/g08.txt`). The ids were right |
| `g09` | Passed. The grammar with existing category names was accepted |
| `g10` | Passed. 7 added, 6 analyzed and sorted and binned, `broken.jpg` Failed, every hash matched. 116 s |
| `m01`, `m02`, `m03` | **Skipped: this PC has only drive C:.** Not counted as passed |

### g07: where the time goes

15.8 s and 15.6 s per photo, of which about 0.8 s is writing the reply. A 1,000 photo project takes about 262 minutes.

A hand run with timestamps (`output/hand-infer-stdout.txt`, `output/hand-infer-stderr.txt`) splits one 15.0 s call like this:

- about 6 s from start until the model is loaded,
- 2.9 s for the work itself (2.4 s reading the image, 0.15 s prompt, 0.34 s reply),
- **about 6 s after the result is printed before `geniex` exits.**

So loading and unloading are about 80% of every call. One command per item works, but it is slow, and nearly half of the overhead comes after the answer is already on stderr. Two things the owner could decide on: take the result as soon as the stderr line arrives and let the process finish while the next item is prepared (two `geniex` processes must still never overlap on the NPU), or ask GenieX for a way to keep a model loaded between calls.

## By hand

- **Dev server, real models.** Settings shows the version, the path and both models. A project from a folder of 9 generated files: 7 analyzed and sorted into 4 categories, `empty.mp4` and `still_truncated.jpg` Failed with "Couldn't read this file", status line went Describing, Encoding frames, All caught up. An H.264 `.mp4` and an HEVC `.mov` both analyzed, with FFmpeg cutting the frames.
- **Bin Files.** 7 files moved into 4 category folders; SHA-256 of all 9 files the same before and after; the 2 failed files untouched where they were.
- **A run interrupted for real.** 200 copied files (623 MB), simulated model. Bin Files started, the server process killed with 50 files moved and 150 not. Started again: the run finished by itself in 6 s, the report says "200 files binned", folder check OK, and all 200 hashes match.
- **Installer.** `tauri build` made `Agentic Binning_1.0.1_arm64-setup.exe` (4.08 MiB). Installed once for the current user. The installed app starts, its window and WebView2 come up, and it closes cleanly.

## Fixed

| Commit | What |
| --- | --- |
| `916b950` | The top bar printed the word "null" where the Simulated tag goes. It only shows with real GenieX, so it was never seen on Linux. `append` and `replaceChildren` now skip null as `h()` does (`ui/dom.js`) |
| `1d11e9e` | `parse_stdout`, the fallback used when the `--test-mode` line is missing, kept GenieX's status lines (`loading model...`, `Model loaded: ...`, `encoding...`) as part of the reply. The normal path was never affected. The parser tests now include the real v0.10.0 output, and the notes at the top of `geniex.rs` say what was observed |
| `605a7f4` | `drives_the_geniex_command_line` runs on Windows too: the stand-in for GenieX is now a small Rust program instead of a Python script |

## Still open

1. **Two drives and exFAT** (`m01` to `m03`): needs a second NTFS drive and an exFAT stick. `$env:AB_OTHER_DRIVE = "D:\ab-test"`, then the command in the brief.
2. **The window itself, by hand:** the folder and file pickers, dragging files onto the window, the title bar and resizing, keep-awake during a run, and video frames without FFmpeg (take FFmpeg off the PATH, try one H.264 `.mp4` and one HEVC `.mov`). The window's frame reader has still never run.
3. **Things only a person can cause:** a file open in a video player during Bin Files, a OneDrive folder as the source, sleep during a run.
4. **The installer puts the program where the app keeps its data.** Both are `%LOCALAPPDATA%\Agentic Binning`: `agentic-binning.exe` and `uninstall.exe` sit beside `settings.json`, `work` and `scratch`. Nothing broke, but what uninstalling does to the settings and project list was not tested. Worth a decision before release.
5. **`g04`'s note:** find out whether the model is given all frames of a clip. In the dev server the two test clips got sensible descriptions, but both were short.
6. **Speed** (above).
7. **Friendly names end in "Photo" or "video"** ("Minimalist Red Square Photo", "Color test pattern video"). The sorting model's choice; the owner may want the prompt to forbid it.
8. Adding 200 photos took about 35 s (about 6 a second) before analysis began.
9. The screenshots in `screens/` are from the dev server in a browser, not from the window, and cover Analyze and Project Media in light and dark, plus Settings, the Bin Files sheet and its report in dark only.
10. The Claude API key store was not touched, and no Claude call was made.
