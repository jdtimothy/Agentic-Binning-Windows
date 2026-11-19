# Changelog

All notable changes to Agentic Binning for Windows are listed here. The project
uses [Semantic Versioning](https://semver.org/). Version numbers follow the
Android app's where the features match.

## [1.0.1] — unreleased

First Windows version, at feature parity with Agentic Binning for Android 1.0.1.
Built and tested on Linux and on GitHub's Windows ARM64 machine with a simulated
model, then run on a Snapdragon X2 Elite Extreme PC with GenieX CLI v0.10.0
(`docs/notes/windows-first-run.md`). The window's own pickers, drag and drop
and frame reader are not yet checked there.

- Projects, adding folders and files (also by drag and drop), analysis with
  LFM3-VL-2B through the GenieX command line, sorting with LFM3-1.7B or Claude.
- One `geniex serve` is kept running for a run, so the model is loaded once
  instead of for every file: about 2 seconds a photo instead of 16.
- A video's actions sit where the picture changes and carry that frame's own
  time: the app picks the moments and asks the model about each in a short
  call. Before, every clip's actions were bunched in its first seconds.
- Analyze and Project Media screens, search, filters, editing, Re-sort,
  category rename and merge, the failed-item sheet.
- Bin Files with the folder check, checked copies, pause, stop, resume after a
  crash, and the report.
- GenieX is the only model the app uses; the simulated one is for tests and the
  browser devserver (`--simulated`). When GenieX or FFmpeg is missing, Settings
  says so and offers to install it with winget, only when you press Install.
- Thumbnails for clips copied in from another project while they wait.
- On an ARM64 PC the app picks an ARM64 FFmpeg over an x64 one found first,
  and Settings warns when only an x64 build is installed (about a third
  slower). File hashing uses the processor's SHA-256 instructions.
- A new app icon.
- About 430 automated tests, most of them the Android app's own cases, plus
  tests for real GenieX and a second drive that are run by hand.
