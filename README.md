<p align="center">
  <img src="ui/assets/appicon.png" alt="PutMPV logo" width="250" />
</p>

# PutMPV

PutMPV is a desktop app built in rust with slint for browsing media from [put.io](https://put.io), fetching movie and TV metadata from TMDB, TVMaze, and similar sources, and handling playback with [mpv](https://mpv.io).

Put.io generally provides two links, one for their transcoded MP4 and one for the raw file. The raw file is generally better as it's not transcoded for the umpteenth time, can contain extra audio tracks, subtitles that aren't chosen at random from some subtitle site (and that actually match the video), just to name a few.

Unless you're managing your local media server it can be a bit annoying to track what you've watched if you're not using the builtin video player.

Basically I got tired of watching a TV show and dragging the raw download link into MPV all the time. I cobbled together this and despite how messy it looks - it seems to work alright.

It is built around a simple flow:

- browse your raw Put.io files
- fetch and organize metadata into a cleaner library (the file parser is a work in progress and may have errors)
- open movies and episodes with artwork, summaries, subtitles, and watched state
- manage playback, matching, and sync settings in one place

---

## Installation

### Arch Linux (AUR)

```bash
yay -S putmpv-bin
```

### macOS (Apple Silicon)

```bash
curl -fsSL https://raw.githubusercontent.com/X0RA/Slint_PutMPV/main/scripts/install-macos.sh | bash
```

### Linux / Windows

Grab the latest installer from the [Releases page](https://github.com/X0RA/Slint_PutMPV/releases/latest).
