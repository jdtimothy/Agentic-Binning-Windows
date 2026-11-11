# Third-Party Notices

Agentic Binning's own code is under the MIT License (see [LICENSE](LICENSE)).
The app also uses or downloads the third-party components below, which keep
their own licenses.

## Qualcomm GenieX

- **What:** the GenieX command-line program. The app runs it; it is **not**
  included in this repository or in the app's installer. You install it
  yourself, under Qualcomm's terms.
- **License:** BSD 3-Clause (https://github.com/qualcomm/GenieX/blob/main/LICENSE).
  The QNN / AI Engine Direct runtime libraries that ship inside GenieX are
  Qualcomm proprietary software under the Qualcomm AI Runtime SDK EULA.
- GenieX was developed by Qualcomm Technologies, Inc. and/or its subsidiaries.

## LFM3-VL-2B and LFM3-1.7B models (Liquid AI)

- **What:** the models used for analysis and for sorting.
- **License:** LFM Open License v1.0 (https://huggingface.co/LiquidAI/LFM3-VL-2B).
- The models are **not** distributed with this app. GenieX downloads them from
  Hugging Face under Liquid AI's terms, which apply between you and Liquid AI.

## FFmpeg (optional)

- **What:** if `ffmpeg` and `ffprobe` are installed, the app runs them to read
  video frames and metadata. They are not included with the app.
- **License:** LGPL 2.1 or later, or GPL, depending on the build you install.

## Libraries built into the app

Resolved from crates.io at build time; none are vendored in this repository.

| Component | License |
| --- | --- |
| Tauri, tauri-plugin-dialog, tauri-plugin-single-instance | MIT or Apache-2.0 |
| rusqlite | MIT; bundles SQLite (public domain) |
| image, kamadak-exif | MIT or Apache-2.0; BSD-2-Clause |
| ureq, rustls | MIT or Apache-2.0; ISC |
| serde, serde_json, regex, sha2, chrono, parking_lot, thiserror, base64, fs4, unicode-normalization, keyring, windows-sys, tiny_http | MIT or Apache-2.0 |

On Windows the app shows its interface in Microsoft Edge WebView2, which is
part of Windows 11 and is governed by Microsoft's terms.
