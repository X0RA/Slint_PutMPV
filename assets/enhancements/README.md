# Video enhancements

Playback settings → Enhance contains spatial enhancements alongside Anime4K.
Standard preserves the player’s original scaler, with all optional shaders off.
EWA Lanczos sharp is an explicit opt-in choice. Lightweight
uses spline36. FSRCNNX and Anime4K are mutually exclusive upscalers. CfL Lite,
SSimSuperRes and SSimDownscaler are independent options. The upstream shader
conditions decide when each pass runs; Enabled does not mean every pass is
active at the current video/output size.

Enhancements share Video's Remember for next time setting. Reset in Enhance
resets only enhancements; Video's Reset all resets picture/speed settings without
changing enhancements or their diagnostics.
Existing saved Anime4K selections remain compatible. No scrape is required.

Film grain defaults to Off. The 0–100 slider maps to mpv's built-in
`deband-grain` range 0–256 and requires no additional shader assets. When enabling
the deband pass solely for grain, zero debanding iterations avoid smoothing the
picture. Existing debanding iterations are preserved if already enabled; turning
grain off restores the original enabled state, iterations and grain amount.
This is simple noise texture, not a film-stock simulation. It runs before
upscaling, so restoration/sharpening can soften or emphasise it. Native grain
remains available when bundled shaders are blocked on NVIDIA GLES.

## Sources and distribution

`manifest.json` records exact upstream revisions/URLs and SHA-256 digests.
GLSL source is bundled and embedded into the binary. CfL Lite has a documented
compatibility adaptation: GLSL 330 array constructors and its upstream non-gather
sampling path. The unmodified pinned source is retained under `upstream/`; the
manifest records both hashes. All other shaders are unmodified. At runtime the
sources and licences are installed under the app config directory's
`shaders/enhancements/<bundle hash>/`. Tests check hashes against the manifest.
Existing custom shader paths are preserved. Only paths under the application's
own Anime4K and enhancement directories are replaced.

- FSRCNNX x2 8-0-4-1 and 16-0-4-1, igv, release 1.1:
  https://github.com/igv/FSRCNN-TensorFlow/releases/tag/1.1
  LGPL-3.0-or-later, as stated in the released shader headers.
- SSimSuperRes, Shiandow / igv:
  https://gist.github.com/igv/2364ffa6e81540f29cb7ab4c9bc05b6b
  LGPL-3.0-or-later.
- SSimDownscaler, Shiandow / igv:
  https://gist.github.com/igv/36508af3ffc84410fe39761d6969be10
  LGPL-3.0-or-later.
- CfL Prediction Lite, João Chrisóstomo:
  https://github.com/Artoriuz/glsl-chroma-from-luma-prediction
  MIT; see CfL-LICENSE and the shader header.

LGPL-3.0.txt and GPL-3.0.txt accompany the LGPL sources. These shaders retain
their upstream licences; they are not relicensed under the application's licence.

## Rendering constraints

The renderer remains embedded libmpv OpenGL. FSRCNNX enlarges luma by 2x only
when output scaling exceeds 1.3x on both axes; mpv completes the final resize.
SSimSuperRes refines POSTKERNEL output and may emphasise grain; it is optional.
CfL Lite reconstructs subsampled chroma and can be useful at native video size.
Full CfL's current PARAM directives require gpu-next, so it is not bundled.
SSimDownscaler uses Mitchell downscaling with linear-downscaling=no, restoring
the previous options when disabled. Its hooks run only for downscaling.

The NVIDIA GLES restriction applies to all bundled enhancement shaders, retaining
requested settings. Built-in scalers (including Lightweight) remain effective;
neural upscalers fall back to the player’s original scaler. Shader errors are
reported once without blaming or removing external shaders. Dropped frames are
monitored; production GPU-time claims are deliberately absent. Changing a shader
selection clears the sticky diagnostic and retries. No automatic quality changes.

Motion interpolation is not exposed here. The current Slint render integration
has no actual presentation/swap feedback or display-refresh negotiation; enabling
mpv interpolation without that work would not provide reliable judder removal.

## Validation

Run focused tests from the project folder, using the ordinary target directory:

```sh
cargo test --profile dev-fast --features slint/mcp player::settings::tests
cargo test --profile dev-fast --features slint/mcp player::enhancements::tests
# Requires ffmpeg and a working desktop; renders subsampled neutral patches.
cargo test --profile dev-fast --features slint/mcp enhancements_colour_smoke -- --ignored --nocapture
ffmpeg -f lavfi -i testsrc2=size=640x360:rate=24 -t 12 -c:v libx264 \
  -preset ultrafast -pix_fmt yuv420p /tmp/putmpv-enhancements-sample.mp4
PUTMPV_ENHANCEMENTS_SAMPLE=/tmp/putmpv-enhancements-sample.mp4 \
  cargo test --profile dev-fast --features slint/mcp enhancements_gpu_smoke -- --ignored --nocapture
SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT=9316 \
  cargo run --profile dev-fast --features slint/mcp --example enhancements-preview
```

The GPU check exercises individual enhancements, both neural models combined
with refinement/chroma, Anime4K with refinement/chroma, actual downscaling and
returning to the original custom shader. Trace timings are diagnostic only.
Windows/macOS and other GPU drivers still require hardware validation.

Validation on 2026-10-18: Linux, NVIDIA GTX 1070, driver 580.178.04,
embedded desktop OpenGL 3.3. All eleven GPU stages passed without renderer
errors. Individual active passes were FSRCNNX Balanced 11, High 21,
SSimSuperRes 4, CfL Lite 2, SSimDownscaler 4. The 640x360 → 1280x720 test
reported median GPU frame times around 1.57 ms (Balanced) and 2.74 ms (High).
These small synthetic fixtures are not a 4K performance guarantee. Neutral YUV
patch framebuffer checks passed for both models and combined chains. UI selection,
mutual exclusion, toggles, reset and tab navigation were exercised through Slint
MCP using the standalone preview.
