# WAVE

WAVE is a small macOS Toybox/Radiant audio effect for inspecting kicks and
other transients while listening in a DAW. It passes stereo audio through
unchanged and displays the latest completed selected beat-window waveform when the host
provides tempo, musical position, and playback state.

When the host does not provide complete transport information, WAVE labels the
view explicitly and falls back to a bounded rolling window. It never calls the
fallback view synchronized.

## Build and test

The framework dependencies are pinned to the validated Toybox/Radiant commits
in `Cargo.toml`.

```bash
bash scripts/ci.sh
TOYBOX_UI_SCREENSHOT=1 bash scripts/ci.sh --screenshots
VST3_SDK_DIR=/Users/portalsurfer/lib/vst3sdk bash scripts/ci.sh --vst3
```

To create fresh host-installable macOS CLAP and VST3 bundles in the audiodev
root `dist/` directory:

```bash
VST3_SDK_DIR=/Users/portalsurfer/lib/vst3sdk bash scripts/build-macos-test.sh
```

The script ad-hoc signs and audits both bundles. They are local test artifacts,
not notarized release packages.

## Production releases

The same producer is used locally and by the manual `WAVE release` Actions
workflow. On a clean macOS arm64 checkout, with `VST3_SDK_DIR` set to the
pinned Steinberg SDK and the Apple Developer ID/notarization credentials
configured, run:

```bash
bash scripts/release.sh --package-only --channel stable
```

Production releases select the next unused global patch version across stable,
RC, and nightly channels, commit that version to `main`, and build from the
resulting exact source commit. Nightlies use the same serialized release path,
so a published nightly also advances the patch version.

This creates `dist/releases/wave-v<version>-<12-char HEAD>/` containing the
host-installable `wave-v<version>-macos.clap.zip` and
`wave-v<version>-macos.vst3.zip` bundles, `wave-default-960x600.png`,
`CHANGELOG.md`, and a schema 2 `release-manifest.json`. Add `--publish` and set
`PORTALSURFER_RELEASE_TOKEN` in the environment to capability-check and publish
the immutable bundle through
`https://portalsurfer.org/plugins/api/v1/products/wave/releases`. The token is
never accepted as a command-line argument.

`--package-only` is still a production release: it signs, notarizes, staples,
and verifies notarization on both bundles. The Actions workflow requires the
Apple signing/notary secrets, `RADIANT_REPO_TOKEN`, and
`WAVE_RELEASE_UPLOAD_TOKEN` for publish runs.

Production artifacts are macOS arm64, hardened-runtime Developer ID signed,
notarized, stapled, and checked with
`codesign -vvvv -R=notarized --check-notarization`. The release producer
re-audits the final ZIP bytes, bundle metadata, signatures, architecture,
exports, and manifest hashes before the staged upload is atomically committed.

## V1 behavior

- One stereo input and output, exact f32 pass-through, zero latency and tail.
- A native per-instance window selector offers exactly `1 beat` (default),
  `2 beats`, `4 beats (1 bar in 4/4)`, and `8 beats (2 bars in 4/4)`. Synced
  windows align to integer multiples of the selected beat count from project
  beat zero.
- Each selected window is shown as a 1,024-column peak-preserving min/max
  envelope so narrow kicks stay visible at detail.
- While playing, a separate live preview progressively overwrites the captured
  prefix over the prior visible envelope and updates at most 60 times per
  second; it never advances the completed-frame revision or blanks the chart.
- Beat grid and waveform come from one coherent audio-thread publication; the
  displayed lane keeps its own window metadata while a newly selected window
  assembles.
- Stop holds the last complete frame; selection changes, seek, loop wrap, tempo
  change, and discontinuity discard only partial/live capture. The GUI keeps
  drawing the latest available envelope while the next complete window starts.
- No parameters, sidechain, MIDI, spectrum, export, zoom, or history controls.

Manual DAW scan, editor resize, transport, and audible pass-through acceptance
remain host-side checks after installing the fresh bundles.
