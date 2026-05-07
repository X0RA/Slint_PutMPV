# PutMPV

A desktop player for [put.io](https://put.io) with embedded mpv playback, TMDB metadata, and watch-state sync — built in Rust + Slint.

## Install

### macOS (Apple Silicon)

```sh
curl -fsSL https://raw.githubusercontent.com/X0RA/Slint_PutMPV/main/scripts/install-macos.sh | bash
```

Downloads the latest `.dmg` from GitHub Releases and copies `PutMPV.app` into `/Applications`. The DMG is ad-hoc signed (no Apple Developer ID), so the script also strips the quarantine attribute — without that, Gatekeeper blocks the app on first launch. If you'd rather not pipe a script to bash, [download the DMG manually](https://github.com/X0RA/Slint_PutMPV/releases/latest) and drag the app across yourself.

### Linux / Windows

Grab the latest binary or installer from the [Releases page](https://github.com/X0RA/Slint_PutMPV/releases/latest).
