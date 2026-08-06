# WAVE project contract

## Goal

Provide a low-friction beat-synced waveform viewer for inspecting kicks and
other transient material without changing the audio being monitored.

## Definition of done for this slice

- CLAP and macOS VST3 expose one stereo effect bus and exact f32 pass-through.
- A valid host tempo plus musical position produces only completed snapshots
  for the selected `1 beat`, `2 beats`, `4 beats (1 bar in 4/4)`, or `8 beats
  (2 bars in 4/4)` window, aligned to integer multiples of that beat count from
  project beat zero.
- During playback, a separate bounded live preview may show the captured prefix
  before that completed snapshot is published; its uncaptured suffix retains the
  prior visible envelope while the new data is progressively written. Clearing
  the live preview never clears the last completed envelope shown by the editor.
- Missing position or tempo is visible in the UI and uses a bounded rolling
  window rather than a false synchronized label.
- Playback stop holds the last complete snapshot; selection changes, seek, loop,
  restart, tempo change, and discontinuity clear only partial/live capture and
  cannot splice partial windows into the completed display. The editor continues
  drawing the latest valid live or completed envelope without a blank interval.
- Audio processing allocates no memory, takes no blocking lock, and does not
  call GUI, filesystem, logging, or host callbacks after activation.
- Radiant’s retained editor renders the waveform, strong whole-beat and soft
  quarter subdivisions across the displayed window, current phase, transport
  label, native window selector, and a high-resolution min/max transient
  envelope.
- Fresh ad-hoc signed CLAP/VST3 bundles can be audited and handed to a user for
  DAW validation.

## Non-goals

V1 does not include DSP, spectrum analysis, sidechain input, MIDI, export,
persistent presets, parameter automation, zoom, freeze, time-signature
decoding, or multi-beat history. The four labels above are explicit 4/4
descriptions, not host time-signature decoding.

## Ownership boundaries

- `src/capture.rs` owns bounded audio-side capture and atomic coherent snapshot
  publication.
- `src/gui.rs` owns the retained Radiant projection and paint plan only.
- `src/vst3/` owns VST3 lifecycle, transport decoding, raw buffer validation,
  and the Toybox runtime handoff.
- The plugin crate owns only WAVE-specific behavior; Toybox and Radiant own
  host and GUI infrastructure.
