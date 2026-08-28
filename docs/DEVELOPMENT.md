# WAVE development notes

## Realtime contract

`CaptureEngine` reserves a logical sample capacity during activation; the raw
sample buffer is intentionally unnecessary because only fixed 1,024-bin
min/max envelopes are retained. The process path updates atomics and publishes
into one of two preallocated envelope slots. A separate two-slot live lane
publishes at most one captured envelope per process block and is bounded to a
60 Hz cadence. The GUI reads both lanes and the live/transport metadata with
bounded sequence validation. Window selection is one independent per-instance
atomic and is latched once at each block boundary; GUI writes only that atomic,
while the audio-owned active value is copied into each lane's metadata before
release. The VST3 process path validates bus shape, channel pointers,
range overflow, and aliasing before touching host memory.

Selectable synced lengths are one, two, four, or eight quarter-note beats,
with explicit labels `4 beats (1 bar in 4/4)` and `8 beats (2 bars in 4/4)`.
The logical capacity covers the largest supported case: 8 beats at 20 BPM and
192 kHz is 4,608,000 samples.

Exact per-channel in-place aliases are accepted. Cross-channel overlap and
partial overlap are rejected. When a writable stereo output is structurally
valid but the input descriptor is malformed, the output is silenced before the
callback returns an invalid-argument result.

## Transport policy

The host position is treated as the start of the current block. The capture
engine compares the next block position with the expected end of the previous
block using a bounded beat tolerance. A larger jump resets the partial window.
Synced windows begin only at integer multiples of the selected beat count from
project beat zero, including floor-safe negative positions. Startup material
inside a window is ignored until the next aligned boundary. The first complete
selected interval is the first snapshot eligible for publication; partial
startup material is never shown as a completed window.

Invalidating a partial or live capture changes only current-live validity. The
last valid live slot remains a display-only retained prefix with its capture
metadata. During live capture and after invalidation, the editor overlays that
prefix over the tail of the prior completed envelope when one exists; without a
completed frame, it is shown alone. It remains explicitly non-completed: it
never advances the completed revision or becomes the current aligned frame.
The editor presents concise LIVE, HELD, or WAITING status while the
retained/non-completed internal state remains unchanged.

## Validation ladder

Run focused Rust tests and Clippy first, then the full default CI, VST3 CI, and
Radiant screenshot gate. Build fresh bundles before handing the device to a
user. The user owns final DAW scan, editor lifecycle, transport, resize, and
audible pass-through acceptance in their active host.
