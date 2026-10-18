# Bundled Anime4K shaders

Upstream: https://github.com/bloc97/Anime4K

Revision: `7684e9586f8dcc738af08a1cdceb024cc184f426`.

The 14 unmodified shaders implement upstream's six Low-end (Fast) and six
High-end (HQ) mpv presets. `manifest.json` records their original paths, Git blob
hashes and SHA-256 checksums. The CNN and Clamp shaders carry MIT notices; the
AutoDownscale helpers carry Unlicense/public-domain notices. Keep those headers
and the upstream `LICENSE` when updating the bundle.

PutMPV embeds these files in the executable and installs verified copies under
its configuration directory at `shaders/anime4k/<revision>/`. No download is
required. The first successful install is verified against the embedded bytes.
Subsequent file loads use the cached installation, checking only that the files
still exist. Missing files trigger repair; a shader error invalidates the cache
so the next application rechecks and repairs changed files. Old revision folders
are retained for now, including for other running app instances.

Playback settings → Enhance → Anime4K enables the feature. Defaults are Off, A,
and Fast. The selection follows the Video section's session/remember behavior;
Reset all returns it to Off. All files can use the effect without metadata.

The ordered Anime4K chain follows existing custom shaders. Turning it off only
removes entries inside PutMPV's own Anime4K directory. Native mpv arrays preserve
paths containing colons, semicolons, backslashes, spaces and Unicode.

The shared Slint/libmpv context explicitly requests desktop OpenGL. Slint's
default GLES context can corrupt CNN calculations on NVIDIA drivers even when
all shader passes compile successfully; HQ B and B+B show a strong red cast,
and the other HQ presets also shift colours. See [mpv issue 18566](https://github.com/mpv-player/mpv/issues/18566).
This retains FemtoVG and mpv's embedded OpenGL renderer, and leaves the bundled
upstream shader files intact. Changing mpv's `opengl-es` option alone would not
change the context owned by Slint.

Slint/glutin already retry context creation with default attributes if the
requested desktop context fails. The renderer checks the actual GL version and
vendor and logs GLES use. NVIDIA GLES blocks Anime4K with a visible explanation;
the requested toggle/preset remains remembered but its shaders are not applied.
Unrelated settings saves preserve that preference, and a compatible context can
apply it again. Other GLES vendors can continue to use it. Ordinary playback and unrelated
custom shaders remain available. This guard does not change the renderer.

mpv's shader errors do not reliably identify the failing shader. They therefore
show one general video-shader warning per failure episode, ignoring repeated
source-dump messages until a retry. This does not disable Anime4K or rewrite the
saved selection. The warning survives file loads and unrelated settings edits;
an accepted enhancement change or Enhance Reset clears it. Video Reset preserves
the selection and its diagnostic.

Production status is "On", with a warning for newly dropped frames. There
is no GPU-time display or time-based slow-rendering heuristic: `vo-passes` is
unavailable without advanced-control mode, which would require a different
threading design. Previews reflect these actual states. The opt-in
`anime4k_gpu_smoke` test uses trace pass reports for diagnostics only and checks
all presets, GPU frame times, and preservation of a custom shader. Production
mpv logging stays at error level.

Run the optional GPU check with a generated 640×360 sample (1280×720 render target):

```sh
ffmpeg -f lavfi -i testsrc2=size=640x360:rate=24 -t 12 -c:v libx264 \
  -preset ultrafast -pix_fmt yuv420p /tmp/putmpv-anime4k-sample.mp4
SLINT_BACKEND=winit-femtovg PUTMPV_ANIME4K_SAMPLE=/tmp/putmpv-anime4k-sample.mp4 \
  cargo test --profile dev-fast --features slint/mcp anime4k_gpu_smoke -- --ignored --nocapture
```

The separate colour regression check generates neutral patches, renders all 12
presets into the real player framebuffer, and compares RGB values with Off.
It also rejects blank output. Run it separately (each test owns a desktop event
loop):

```sh
SLINT_BACKEND=winit-femtovg \
  cargo test --profile dev-fast --features slint/mcp anime4k_colour_smoke -- --ignored --nocapture
```

For diagnosis, `PUTMPV_TEST_GL=es` selects the old GLES path and reproduces the
failure on affected NVIDIA drivers. Failed captures remain in the temporary
directory printed by the assertion.

For a standalone mock preview with MCP support, run from the project directory:

```sh
SLINT_BACKEND=winit-femtovg SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT=9316 \
  cargo run --profile dev-fast --features slint/mcp --example anime4k-preview
```

The renderer remains `vo=libmpv`. No PiP or window-management changes are part
of this feature.
