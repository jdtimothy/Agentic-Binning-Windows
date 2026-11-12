# First run on a Snapdragon X PC

11 November 2026, following `docs/windows-agent-brief.md`. Saved outputs are in `output/`, screenshots in `screens/`.

## Result

The app's core works on this PC with real GenieX. Every automated and on-device test passed, including the moves between two drives once the owner attached an exFAT drive. Not done: a second NTFS drive, and the checks that need the app's own window driven by hand.

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
| `m01`, `m02`, `m03` | Skipped at first (one drive). Passed later the same day with `AB_OTHER_DRIVE` on the owner's exFAT drive D: (931 GB, 56 GB free): both directions copy, check, place and delete each file, the folder check is OK, and a rename on exFAT refuses to replace a file (`output/moves-exfat.txt`). A second NTFS drive is still untested |

### g07: where the time goes

15.8 s and 15.6 s per photo, of which about 0.8 s is writing the reply. A 1,000 photo project takes about 262 minutes.

A hand run with timestamps (`output/hand-infer-stdout.txt`, `output/hand-infer-stderr.txt`) splits one 15.0 s call like this:

- about 6 s from start until the model is loaded,
- 2.9 s for the work itself (2.4 s reading the image, 0.15 s prompt, 0.34 s reply),
- **about 6 s after the result is printed before `geniex` exits.**

So loading and unloading are about 80% of every call. One command per item works, but it is slow, and nearly half of the overhead comes after the answer is already on stderr. Two things the owner could decide on: take the result as soon as the stderr line arrives and let the process finish while the next item is prepared (two `geniex` processes must still never overlap on the NPU), or ask GenieX for a way to keep a model loaded between calls.

### After the switch to `geniex serve`

The owner chose to keep the model loaded, as the Android app does (it loads once per run in `PipelineController.runBatch` and releases before sorting). The app now starts one `geniex serve` and sends every call to it (`crates/core/src/geniex_serve.rs`). The same tests, same day, same PC (`output/serve-g01-g10.txt`):

| | One `geniex infer` per call | Through `geniex serve` |
| --- | --- | --- |
| `g07`, one small photo | 15.8 s and 15.6 s | 1.4 s and 1.3 s |
| `g10`, 7 files analyzed, sorted and binned | 116 s | 38 s |
| `g01` to `g10` | all pass | all pass |

In the dev server (optimised build), a folder of 20 photos of 4000 x 3000 and two 6-second 1080p clips, real models:

- loading the vision model: about 8 s, once;
- each photo: about 1.8 s, so about 30 minutes for 1,000 photos instead of about 262;
- each clip: about 26 s (16 frames);
- sorting all 22: about 24 s, of which about 19 s is the server swapping to the sorting model and the first call;
- everything: 124 s.

A debug build takes about 7 s a photo of that size, nearly all of it decoding the JPEG, so time only optimised builds.

Also checked: the server stops when the app closes; it ends with the app when the app is ended from outside (a Windows job object; seen when the dev server and the test program were ended without a clean close); a settings change while nothing runs stops it; Stop returns at once and the next call works (`g06`).

### Real video

The owner gave six real clips: 28 to 62 seconds, 1080p and 4K, 24 to 60 frames a second, five H.264 `.mp4` (up to 94 Mbit/s) and one HEVC `.m4v`. Copies were analyzed in the dev server, through `geniex serve`; the originals were only read, and their hashes were the same afterwards.

- **All six were analyzed and sorted**, the HEVC one too. The descriptions name what is in the clips, including scenes from late in them, so the model is given frames from the whole clip.
- **The action times are wrong.** A 62-second clip gets actions at 0:00, 0:03, 0:07, 0:11 and 0:15: the model labels the moments it picks with the first few timestamps of the list. Tried by hand against the server, with the app's grammar, on two clips: the list as now, Android's layout (a `[0m5.00s]` stamp between frames every 5 s), the list with a sentence asking for the whole clip, and whole-second times. None spread the times over the clip, Android's layout included, so the prompt was left as it is. The grammar also only allows `m:ss` while the listed times have tenths (`0:01.9`).
- **Where a clip's time went:** about 20 s cutting 16 frames, one FFmpeg run per frame. The FFmpeg winget installs (`Gyan.FFmpeg`) is an x64 program, emulated on this PC, and takes 1.3 s only to start. One run that opens the clip once per frame cuts the same frames (same bytes) in 5 s for 1080p and 10 to 12 s for 4K. That is now what the app does. An ARM64 build of FFmpeg would be faster still; not installed, because it is not the build the brief names.
- **The model's part:** about 12 s reading 16 frames and about 9 s writing some 300 tokens.
- **Writing speed dropped during the afternoon** from 68 to 70 tokens a second to 31 to 42, for photos as well. A newly started `geniex serve`, asked about one photo with no grammar and any reply length, gave the same 42, and naming the power mode (`burst`, `high_performance`) changed nothing. So it is the PC's state after about an hour of load, not the app; the PC was on mains power on the Balanced plan, and its temperature could not be read. Worth measuring again on a cold start.

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
| `2627069` | One `geniex serve` is kept running, so the model is loaded once per run instead of for every file (above) |
| `8e341f8` | Cards of files that wait to be analyzed were blank until the model reached them (the owner saw this while testing). A background thread now writes their thumbnails as soon as files are added, in the order they will be analyzed: 22 files had theirs about 3 s after they were added. A video without FFmpeg still gets its thumbnail with its analysis |
| this commit and the two before it | Seen by the owner while testing real clips. The scan line started again from the top every time a video frame was read, because the card is redrawn; it now takes its place in the sweep from the clock. A clip's frames are cut in one FFmpeg run instead of one per frame (above). The Analyze screen has the Android app's backdrop: the picture in view, shrunk to 20 px and blurred, stretched over the window, fading into the next over 2.3 s and drifting while items are analyzed (`ThumbnailBackdrop.kt` is the reference) |

## Still open

1. **A second NTFS drive** (`m01` to `m03` passed on exFAT only): `$env:AB_OTHER_DRIVE = "E:\ab-test"`, then the command in the brief.
2. **The window itself, by hand:** the folder and file pickers, dragging files onto the window, the title bar and resizing, keep-awake during a run, and video frames without FFmpeg (take FFmpeg off the PATH, try one H.264 `.mp4` and one HEVC `.mov`). The window's frame reader has still never run. Seen since, in the installed app, with the owner driving and FFmpeg present: the folder picker, 14 real clips (3 s to 73 s) added with thumbnails, the app's own `geniex serve`, all 14 analyzed and sorted in 3 min 35 s (about 15 s a clip, 50 tokens a second), the backdrop, and "All caught up". The status line read "No media yet" while the clips were being added. A 73-second clip got actions at 0:00, 0:04, 0:09, 0:13 and 0:18, which are exactly its first five frame times (item 5).
3. **Things only a person can cause:** a file open in a video player during Bin Files, a OneDrive folder as the source, sleep during a run.
4. **The installer puts the program where the app keeps its data.** Both are `%LOCALAPPDATA%\Agentic Binning`: `agentic-binning.exe` and `uninstall.exe` sit beside `settings.json`, `work` and `scratch`. Nothing broke, but what uninstalling does to the settings and project list was not tested. Worth a decision before release.
5. **Action times of a video** are not spread over the clip (Real video, above). The model sees all the frames; it does not use their times. No prompt layout tried fixes it. Tried since, by hand against the server on two real clips: printing each frame's time into the picture (the model still wrote the first five times in order), and fixing five evenly spread times in the grammar so the model writes only the text (the text then does not match what those frames show). What does work is one short call per key frame, asking what that frame shows, with the app supplying the frame's time: five frames took 5.5 s more per clip and each line matched its frame. That changes the prompt, the number of calls and what an "action" is (a caption of an evenly spaced frame, not a moment the model chose), so it waits for the owner's decision.
6. **Speed:** a clip takes 26 to 35 s (frames 4 to 10 s, the model about 21 s at the slower writing speed). The writing speed itself wants measuring again on a cold PC (above).
7. **Friendly names end in "Photo" or "video"** ("Minimalist Red Square Photo", "Color test pattern video"). The sorting model's choice; the owner may want the prompt to forbid it.
8. Adding 200 photos took about 35 s (about 6 a second) before analysis began.
9. The screenshots in `screens/` are from the dev server in a browser, not from the window, and cover Analyze and Project Media in light and dark, plus Settings, the Bin Files sheet and its report in dark only.
10. The Claude API key store was not touched, and no Claude call was made.
