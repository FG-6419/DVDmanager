# DVDmanager

**English** | [Deutsch](README.de.md)

DVDmanager is a Windows program for **your own, unprotected video DVDs**. It shrinks DVDs to fit a blank disc, rearranges titles, cuts them and writes the result as a DVD, an ISO image or a video file. Video and audio are not re-encoded, except at frame-accurate cut points and fades.

The program deliberately does not process copy-protected DVDs (CSS).

## Features

**Shrink (whole DVD)**
- Fits a DVD 9 onto a DVD 5 blank (4.7 GB) or onto a custom target size.
- Video is requantized instead of re-encoded; menus and structure are kept.
- For each title, audio track and subtitle you choose what to keep and how much to shrink.

**Re-author**
- Combine titles and chapters into a new DVD in your own order.
- Cut at keyframes (no re-encoding) or frame-accurately.
- Fades from and to black or white, optionally including the audio.
- Scene detection: split a title at scene changes.
- Chapter editor with your own chapter markers. Optionally every cut starts a chapter, and all cuts play as one title.
- Clean audio at cut points: partial audio frames are removed, and a short fade prevents clicks.

**Sources**
- DVD drive, VIDEO_TS folder, ISO image.
- Single VOB and MPEG-2 files. They are repacked into a DVD structure without re-encoding.

**Output**
- DVD folder (VIDEO_TS), ISO image, or straight to a blank disc (via the Windows disc burner).
- A single VOB file, a plain MPEG-2 file or an MKV file with chapters.
- After the export, the output is checked automatically for broken audio frames and time jumps.
- Optionally an overview picture (JPG) with thumbnails at an adjustable interval.

**Usage**
- Preview with audio and timeline, "Play all cuts" and "Listen to transition".
- Save and load projects, queue for several DVDs.
- User interface in German and English, light and dark.

## Building

You need [Rust](https://www.rust-lang.org/) (stable).

```bash
cargo build --release
```

The program is then at `target/release/dvdmanager.exe`. Run the tests with `cargo test`; they use a built-in sample DVD.

## Changes

What changed in which version is listed in [CHANGELOG.md](CHANGELOG.md) (in German) and in the program under "About".
