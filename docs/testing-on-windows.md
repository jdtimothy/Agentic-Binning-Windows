# Testing on a Snapdragon X PC

This app was written and tested on Linux with a simulated model. This page is the order to check things in on real hardware, most likely failure first. Use test media you made or copies: Bin Files moves files and has no undo.

## 1. Does it build and start

1. Take the installer from the latest run of the **Build** workflow on GitHub (artifact `agentic-binning-windows-arm64`), or run `tauri build` in `src-tauri`.
2. Start the app. The top-left chip should read **LFM3-VL-2B / NPU**. If it reads **Simulated**, GenieX was not found: open Settings, press **Check again**, or set the path to `geniex.exe`.
3. Settings → GenieX should show the version, its path, and whether each model is downloaded.

If the window is blank, run `tauri dev` from `src-tauri` and look at the terminal and at the web view's console (right-click → Inspect in a debug build).

## 2. The GenieX commands by hand

Before trusting the app, run what it runs. In PowerShell:

```powershell
geniex --version
geniex list --format json
```

`list` must print a JSON array whose entries have `name`, `type` and `precisions`. The app decides "downloaded" from that.

Then a photo. Put any JPEG at `C:\temp\test.jpg` and a prompt file at `C:\temp\prompt.txt` containing exactly two lines:

```text
Describe this photo.
C:/temp/test.jpg
```

```powershell
$env:NO_COLOR = "1"
geniex --skip-update --verbose --test-mode infer LiquidAI/LFM3-VL-2B-GGUF:Q4_0 --compute npu --nctx 16384 --max-tokens 400 --think=false --temperature 0.1 --top-k 50 --top-p 1.0 --min-p 0.000001 --repetition-penalty 1.1 -s "Reply with one JSON object." -i C:\temp\prompt.txt
```

What the app relies on, in the order it would break:

| Check | If it fails |
| --- | --- |
| The command exits by itself after one reply | The app would hang on every item. `-i` must end the session after the file's prompt. |
| The image is found: the reply describes the picture, not "no image" | The CLI did not recognise the path line. Try backslashes; then tell me which form worked. |
| A block ending in `stop reason:    eos` is printed | `--verbose` output changed; the fallback parser needs updating. |
| stderr has one JSON line with `"Output"` and `"Profile"` | `--test-mode` is hidden and may be gone. The app then falls back to the printed text, which should still work. |
| `--grammar-path <file>` is accepted | The app passes a GBNF grammar per call. If GenieX rejects it on Windows, analysis fails with `invalid-json` or an error. |

The app writes the full command of its last call to **Settings → Activity log**. If an item fails, copy that command into a terminal: the error GenieX prints there is the real one.

## 3. A small project

Use `scripts/make-test-media.sh` from the Android repository (it needs FFmpeg) or a dozen of your own files, copied somewhere disposable.

1. **New project** → name it → **A folder** → **Create and analyze**.
2. Watch the Analyze screen: the status line should go Loading model → Reading image (or Encoding frames) → Describing, and the description should type itself out. Tokens per second appear in the middle of the status line.
3. Expected from the Android test set: 7 added; `empty.mp4` and `still_truncated.jpg` end **Failed** ("Couldn't read this file"); the rest analyzed.
4. When analysis ends, the status line shows **Categorizing**, then **All caught up**. Project Media should show category chips.

Things to note down: seconds per photo and per clip (each one loads the model again), and whether a video's actions have sensible times.

## 4. Videos

- With FFmpeg on the PATH, Settings shows its path and frames are cut with it.
- Without FFmpeg, the window itself plays the clip and captures frames. This path could not be tested off Windows at all. Try one H.264 `.mp4` and one HEVC `.mov` from a phone. A clip Windows can't play fails with "Couldn't read this file".

## 5. Editing and search

- Search for a word from a description; a tile shows "Matched in tags" and the like.
- Open an item, change its name and category, Save. Re-sort with **Keep my edits** ticked: they must survive.
- Categories → Edit: rename one, merge two.

## 6. Bin Files

1. In Project Media press **Bin Files**, read the sheet, press **Bin N files**.
2. The files should now be in `Pictures\Agentic Binning\<project>\<Category>\<Friendly name>.<ext>`, and gone from where they were.
3. Try **Pause** and **Stop** during a larger run, then Bin Files again: it carries on.
4. Close the app during a run and start it again: the run should resume by itself.
5. Rename a binned item in the app: its file should be renamed on disk at once.
6. Put a stray file into a category folder and press Bin Files: the app should ask whether to add it or leave it.

Cases specific to Windows worth one try each: a source folder on another drive (copied, checked, then the original deleted), a file open in another program (skipped, reported, untouched), a project on an exFAT drive, and a path longer than 260 characters.

## 7. What to send back

For anything that goes wrong: the Activity log from Settings, the command it shows run by hand with its output, and for file moves the report sheet's text. The project's database is `<project>\.binning\project.db`; it opens in any SQLite browser (`select originalName, status, failureReason from item`).
