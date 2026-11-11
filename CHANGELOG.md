# Changelog

All notable changes to Agentic Binning for Windows are listed here. The project
uses [Semantic Versioning](https://semver.org/). Version numbers follow the
Android app's where the features match.

## [1.0.1] — unreleased

First Windows version, at feature parity with Agentic Binning for Android 1.0.1.
Built and tested on Linux and on GitHub's Windows ARM64 machine with a simulated
model, then run on a Snapdragon X2 Elite Extreme PC with GenieX CLI v0.10.0
(`docs/notes/windows-first-run.md`). Moves between two drives and the window's
own pickers, drag and drop and frame reader are not yet checked there.

- Projects, adding folders and files (also by drag and drop), analysis with
  LFM3-VL-2B through the GenieX command line, sorting with LFM3-1.7B or Claude.
- Analyze and Project Media screens, search, filters, editing, Re-sort,
  category rename and merge, the failed-item sheet.
- Bin Files with the folder check, checked copies, pause, stop, resume after a
  crash, and the report.
- A simulated model when GenieX is not installed, clearly labelled.
- About 430 automated tests, most of them the Android app's own cases, plus
  tests for real GenieX and a second drive that are run by hand.
