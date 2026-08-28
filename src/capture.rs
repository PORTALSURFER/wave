//! Realtime-safe selectable beat-window capture and coherent publication.

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Number of peak-preserving min/max columns sent to the retained editor.
pub const ENVELOPE_BINS: usize = 1024;
/// Maximum capture duration reserved for the largest valid synced window.
pub const MAX_CAPTURE_SECONDS: f64 = 24.0;
/// Maximum sample rate covered by the fixed capture capacity.
pub const MAX_CAPTURE_SAMPLE_RATE: f64 = 192_000.0;
/// Logical sample capacity covered by the largest valid synced window.
pub const MAX_CAPTURE_SAMPLES: usize = (MAX_CAPTURE_SECONDS * MAX_CAPTURE_SAMPLE_RATE) as usize;
/// Fallback rolling-window duration when tempo is unavailable.
pub const FALLBACK_WINDOW_SECONDS: f64 = 0.5;
/// Lower bound accepted for a host tempo.
pub const MIN_TEMPO_BPM: f64 = 20.0;
/// Upper bound accepted for a host tempo.
pub const MAX_TEMPO_BPM: f64 = 300.0;

const MODE_EMPTY: u32 = 0;
const MODE_SYNCED: u32 = 1;
const MODE_UNSYNCED_TEMPO: u32 = 2;
const MODE_UNSYNCED_FALLBACK: u32 = 3;
const POSITION_TOLERANCE_SAMPLES: f64 = 1.5;
const LIVE_PREVIEW_HZ: f64 = 60.0;
const MAX_ENVELOPE_READ_RETRIES: usize = 4;

const WINDOW_ONE_BEAT: u32 = 0;
const WINDOW_TWO_BEATS: u32 = 1;
const WINDOW_FOUR_BEATS: u32 = 2;
const WINDOW_EIGHT_BEATS: u32 = 3;

/// The closed set of beat-window lengths available in the WAVE editor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WindowLength {
    /// One quarter-note beat.
    #[default]
    OneBeat,
    /// Two quarter-note beats.
    TwoBeats,
    /// Four quarter-note beats, one bar in 4/4.
    FourBeats,
    /// Eight quarter-note beats, two bars in 4/4.
    EightBeats,
}

impl WindowLength {
    /// The default window used by every new WAVE instance.
    pub const DEFAULT: Self = Self::OneBeat;
    /// All selectable window lengths in menu order.
    pub const ALL: [Self; 4] = [
        Self::OneBeat,
        Self::TwoBeats,
        Self::FourBeats,
        Self::EightBeats,
    ];

    /// Return the number of quarter-note beats in this window.
    pub const fn beats(self) -> u32 {
        match self {
            Self::OneBeat => 1,
            Self::TwoBeats => 2,
            Self::FourBeats => 4,
            Self::EightBeats => 8,
        }
    }

    /// Return the exact user-facing label for this window.
    pub const fn label(self) -> &'static str {
        match self {
            Self::OneBeat => "1:4",
            Self::TwoBeats => "1:2",
            Self::FourBeats => "1:1",
            Self::EightBeats => "2:1",
        }
    }

    /// Convert this closed domain to its atomic representation.
    pub const fn as_raw(self) -> u32 {
        match self {
            Self::OneBeat => WINDOW_ONE_BEAT,
            Self::TwoBeats => WINDOW_TWO_BEATS,
            Self::FourBeats => WINDOW_FOUR_BEATS,
            Self::EightBeats => WINDOW_EIGHT_BEATS,
        }
    }

    /// Convert an atomic representation, falling back to one beat when invalid.
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            WINDOW_TWO_BEATS => Self::TwoBeats,
            WINDOW_FOUR_BEATS => Self::FourBeats,
            WINDOW_EIGHT_BEATS => Self::EightBeats,
            _ => Self::OneBeat,
        }
    }
}

/// The source of a published waveform snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SnapshotMode {
    /// No complete window has been published yet.
    #[default]
    Empty,
    /// A complete selected beat interval aligned to host beats.
    Synced,
    /// A rolling selected beat-sized window with no host musical position.
    UnsyncedTempo,
    /// A bounded 500 ms rolling window used without a valid host tempo.
    UnsyncedFallback,
}

impl SnapshotMode {
    fn as_raw(self) -> u32 {
        match self {
            Self::Empty => MODE_EMPTY,
            Self::Synced => MODE_SYNCED,
            Self::UnsyncedTempo => MODE_UNSYNCED_TEMPO,
            Self::UnsyncedFallback => MODE_UNSYNCED_FALLBACK,
        }
    }

    fn from_raw(raw: u32) -> Self {
        match raw {
            MODE_SYNCED => Self::Synced,
            MODE_UNSYNCED_TEMPO => Self::UnsyncedTempo,
            MODE_UNSYNCED_FALLBACK => Self::UnsyncedFallback,
            _ => Self::Empty,
        }
    }
}

/// One vertical min/max envelope column.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EnvelopePoint {
    /// Lowest sample in this envelope column.
    pub min: f32,
    /// Highest sample in this envelope column.
    pub max: f32,
}

const EMPTY_CAPTURE_ENVELOPE: EnvelopePoint = EnvelopePoint {
    min: 1.0,
    max: -1.0,
};

/// A transport snapshot supplied to the format-neutral capture engine.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TransportInfo {
    /// Host tempo in beats per minute, when valid and supplied.
    pub tempo_bpm: Option<f64>,
    /// Host project position in quarter-note beats, when supplied.
    pub song_pos_beats: Option<f64>,
    /// Whether the host reports active playback.
    pub is_playing: bool,
}

/// A coherent waveform and transport view copied for GUI consumption.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformView {
    /// Monotonic revision of the completed waveform snapshot.
    pub snapshot_revision: u64,
    /// Snapshot capture mode.
    pub snapshot_mode: SnapshotMode,
    /// Window length used by the completed snapshot.
    pub snapshot_window: WindowLength,
    /// Tempo associated with the published snapshot, if any.
    pub snapshot_tempo_bpm: Option<f32>,
    /// Integer beat index associated with a synced snapshot.
    pub snapshot_beat_index: Option<i64>,
    /// Number of source samples represented by the snapshot.
    pub sample_count: usize,
    /// Min/max envelope columns.
    pub envelope: [EnvelopePoint; ENVELOPE_BINS],
    /// Monotonic revision of the live in-progress preview publication.
    pub live_revision: u64,
    /// Whether the live preview currently contains captured samples.
    pub live_valid: bool,
    /// Capture mode of the live preview.
    pub live_mode: SnapshotMode,
    /// Window length used by the live preview.
    pub live_window: WindowLength,
    /// Tempo associated with the live preview, if any.
    pub live_tempo_bpm: Option<f32>,
    /// Integer beat index associated with the live preview, when synced.
    pub live_beat_index: Option<i64>,
    /// Number of source samples represented by the live preview.
    pub live_sample_count: usize,
    /// Number of source samples in the live preview's target window.
    pub target_sample_count: usize,
    /// Min/max envelope columns for the live preview.
    pub live_envelope: [EnvelopePoint; ENVELOPE_BINS],
    /// Monotonic revision of the last valid display-only live envelope.
    pub display_revision: u64,
    /// Whether a display-only live envelope is retained after invalidation.
    pub display_valid: bool,
    /// Capture mode of the retained display-only live envelope.
    pub display_mode: SnapshotMode,
    /// Window length used by the retained display-only live envelope.
    pub display_window: WindowLength,
    /// Tempo associated with the retained display-only live envelope, if any.
    pub display_tempo_bpm: Option<f32>,
    /// Integer beat index associated with the retained live envelope, when synced.
    pub display_beat_index: Option<i64>,
    /// Number of source samples represented by the retained live envelope.
    pub display_sample_count: usize,
    /// Number of source samples in the retained live envelope's target window.
    pub display_target_sample_count: usize,
    /// Min/max envelope columns retained for display after live invalidation.
    pub display_envelope: [EnvelopePoint; ENVELOPE_BINS],
    /// Current transport tempo, if valid and supplied.
    pub current_tempo_bpm: Option<f32>,
    /// Current transport position, if supplied.
    pub current_song_pos_beats: Option<f64>,
    /// Current transport playback state.
    pub is_playing: bool,
    /// Whether the current transport has musical position data.
    pub timeline_available: bool,
    /// Atomic redraw revision, advanced for every audio block.
    pub redraw_revision: u64,
}

impl Default for WaveformView {
    fn default() -> Self {
        Self {
            snapshot_revision: 0,
            snapshot_mode: SnapshotMode::Empty,
            snapshot_window: WindowLength::DEFAULT,
            snapshot_tempo_bpm: None,
            snapshot_beat_index: None,
            sample_count: 0,
            envelope: [EnvelopePoint::default(); ENVELOPE_BINS],
            live_revision: 0,
            live_valid: false,
            live_mode: SnapshotMode::Empty,
            live_window: WindowLength::DEFAULT,
            live_tempo_bpm: None,
            live_beat_index: None,
            live_sample_count: 0,
            target_sample_count: 0,
            live_envelope: [EnvelopePoint::default(); ENVELOPE_BINS],
            display_revision: 0,
            display_valid: false,
            display_mode: SnapshotMode::Empty,
            display_window: WindowLength::DEFAULT,
            display_tempo_bpm: None,
            display_beat_index: None,
            display_sample_count: 0,
            display_target_sample_count: 0,
            display_envelope: [EnvelopePoint::default(); ENVELOPE_BINS],
            current_tempo_bpm: None,
            current_song_pos_beats: None,
            is_playing: false,
            timeline_available: false,
            redraw_revision: 0,
        }
    }
}

struct SnapshotSlot {
    sequence: AtomicU64,
    revision: AtomicU64,
    mode: AtomicU32,
    window_raw: AtomicU32,
    tempo_bits: AtomicU32,
    beat_index: AtomicU64,
    beat_index_valid: AtomicU32,
    sample_count: AtomicU32,
    mins: [AtomicU32; ENVELOPE_BINS],
    maxs: [AtomicU32; ENVELOPE_BINS],
}

impl SnapshotSlot {
    fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            revision: AtomicU64::new(0),
            mode: AtomicU32::new(MODE_EMPTY),
            window_raw: AtomicU32::new(WindowLength::DEFAULT.as_raw()),
            tempo_bits: AtomicU32::new(0),
            beat_index: AtomicU64::new(0),
            beat_index_valid: AtomicU32::new(0),
            sample_count: AtomicU32::new(0),
            mins: std::array::from_fn(|_| AtomicU32::new(0.0_f32.to_bits())),
            maxs: std::array::from_fn(|_| AtomicU32::new(0.0_f32.to_bits())),
        }
    }
}

struct LiveSlot {
    sequence: AtomicU64,
    revision: AtomicU64,
    // A valid slot remains the display-only retained envelope after the
    // current live capture is invalidated by the publication-level bit.
    valid: AtomicU32,
    mode: AtomicU32,
    window_raw: AtomicU32,
    tempo_bits: AtomicU32,
    beat_index: AtomicU64,
    beat_index_valid: AtomicU32,
    sample_count: AtomicU32,
    target_sample_count: AtomicU32,
    mins: [AtomicU32; ENVELOPE_BINS],
    maxs: [AtomicU32; ENVELOPE_BINS],
}

impl LiveSlot {
    fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            revision: AtomicU64::new(0),
            valid: AtomicU32::new(0),
            mode: AtomicU32::new(MODE_EMPTY),
            window_raw: AtomicU32::new(WindowLength::DEFAULT.as_raw()),
            tempo_bits: AtomicU32::new(0),
            beat_index: AtomicU64::new(0),
            beat_index_valid: AtomicU32::new(0),
            sample_count: AtomicU32::new(0),
            target_sample_count: AtomicU32::new(0),
            mins: std::array::from_fn(|_| AtomicU32::new(0.0_f32.to_bits())),
            maxs: std::array::from_fn(|_| AtomicU32::new(0.0_f32.to_bits())),
        }
    }
}

struct LivePreview<'a> {
    valid: bool,
    mode: SnapshotMode,
    window: WindowLength,
    tempo_bpm: Option<f64>,
    beat_index: Option<i64>,
    sample_count: usize,
    target_sample_count: usize,
    envelope: Option<&'a [EnvelopePoint; ENVELOPE_BINS]>,
}

impl<'a> LivePreview<'a> {
    fn clear() -> Self {
        Self {
            valid: false,
            mode: SnapshotMode::Empty,
            window: WindowLength::DEFAULT,
            tempo_bpm: None,
            beat_index: None,
            sample_count: 0,
            target_sample_count: 0,
            envelope: None,
        }
    }
}

/// Lock-free, double-buffered publication shared by audio and GUI threads.
///
/// Envelope values are atomic because an inactive slot can become stale while
/// a GUI read is still in progress. Sequence validation then gives readers a
/// coherent snapshot without a mutex or a shared mutable non-atomic buffer.
pub struct WaveformPublication {
    slots: [SnapshotSlot; 2],
    active_slot: AtomicUsize,
    snapshot_revision: AtomicU64,
    live_slots: [LiveSlot; 2],
    live_active_slot: AtomicUsize,
    live_active_valid: AtomicU32,
    live_revision: AtomicU64,
    redraw_revision: AtomicU64,
    selected_window_raw: AtomicU32,
    transport_sequence: AtomicU64,
    current_tempo_bits: AtomicU32,
    current_position_bits: AtomicU64,
    current_position_valid: AtomicU32,
    is_playing: AtomicU32,
    timeline_available: AtomicU32,
}

struct PublicationTransaction<'a> {
    publication: &'a WaveformPublication,
    sequence: u64,
}

impl Default for WaveformPublication {
    fn default() -> Self {
        Self::new()
    }
}

impl WaveformPublication {
    /// Create an empty publication.
    pub fn new() -> Self {
        Self {
            slots: [SnapshotSlot::new(), SnapshotSlot::new()],
            active_slot: AtomicUsize::new(0),
            snapshot_revision: AtomicU64::new(0),
            live_slots: [LiveSlot::new(), LiveSlot::new()],
            live_active_slot: AtomicUsize::new(0),
            live_active_valid: AtomicU32::new(0),
            live_revision: AtomicU64::new(0),
            redraw_revision: AtomicU64::new(0),
            selected_window_raw: AtomicU32::new(WindowLength::DEFAULT.as_raw()),
            transport_sequence: AtomicU64::new(0),
            current_tempo_bits: AtomicU32::new(0),
            current_position_bits: AtomicU64::new(0),
            current_position_valid: AtomicU32::new(0),
            is_playing: AtomicU32::new(0),
            timeline_available: AtomicU32::new(0),
        }
    }

    fn begin_publication(&self) -> PublicationTransaction<'_> {
        // The AcqRel RMW publishes the odd marker before any transaction
        // payload is written, while the final Release store publishes it.
        let sequence = self.transport_sequence.fetch_or(1, Ordering::AcqRel);
        debug_assert_eq!(
            sequence & 1,
            0,
            "publication transactions must not be nested or concurrent"
        );
        PublicationTransaction {
            publication: self,
            sequence: sequence | 1,
        }
    }

    /// Return the validated per-instance window selection.
    pub fn selected_window(&self) -> WindowLength {
        WindowLength::from_raw(self.selected_window_raw.load(Ordering::Acquire))
    }

    /// Set the per-instance window selection and request one GUI redraw when it changes.
    pub fn set_selected_window(&self, window: WindowLength) -> bool {
        let previous = self
            .selected_window_raw
            .swap(window.as_raw(), Ordering::AcqRel);
        if WindowLength::from_raw(previous) == window {
            return false;
        }
        self.redraw_revision.fetch_add(1, Ordering::Release);
        true
    }

    /// Record current transport metadata and advance the GUI redraw revision.
    #[allow(dead_code)]
    pub fn update_transport(&self, transport: TransportInfo) {
        let transaction = self.begin_publication();
        transaction.publish_presentation(Some(transport), None);
        transaction.commit();
    }

    /// Publish one live in-progress envelope with its transport presentation.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn publish_live_preview(
        &self,
        transport: TransportInfo,
        mode: SnapshotMode,
        window: WindowLength,
        tempo_bpm: Option<f64>,
        beat_index: Option<i64>,
        sample_count: usize,
        target_sample_count: usize,
        envelope: &[EnvelopePoint; ENVELOPE_BINS],
    ) {
        let transaction = self.begin_publication();
        transaction.publish_presentation(
            Some(transport),
            Some(LivePreview {
                valid: true,
                mode,
                window,
                tempo_bpm,
                beat_index,
                sample_count,
                target_sample_count,
                envelope: Some(envelope),
            }),
        );
        transaction.commit();
    }

    /// Invalidate the live preview without changing the completed snapshot.
    #[cfg(feature = "vst3")]
    pub fn clear_live_preview(&self) {
        let transaction = self.begin_publication();
        transaction.publish_presentation(None, Some(LivePreview::clear()));
        transaction.commit();
    }

    #[allow(dead_code)]
    fn clear_live_preview_with_transport(&self, transport: TransportInfo) {
        let transaction = self.begin_publication();
        transaction.publish_presentation(Some(transport), Some(LivePreview::clear()));
        transaction.commit();
    }

    /// Publish a complete source window from a precomputed min/max envelope.
    ///
    /// Audio-side capture uses this form so completion of a selected window only
    /// copies the fixed envelope, rather than traversing the entire window.
    #[allow(dead_code)]
    pub fn publish_envelope(
        &self,
        mode: SnapshotMode,
        window: WindowLength,
        tempo_bpm: Option<f64>,
        beat_index: Option<i64>,
        sample_count: usize,
        envelope: &[EnvelopePoint; ENVELOPE_BINS],
    ) {
        let transaction = self.begin_publication();
        transaction.publish_envelope(mode, window, tempo_bpm, beat_index, sample_count, envelope);
        transaction.commit();
    }

    /// Copy the latest coherent snapshot and transport metadata into `view`.
    pub fn read_view(&self, view: &mut WaveformView) -> bool {
        for _ in 0..MAX_ENVELOPE_READ_RETRIES {
            let presentation_sequence = self.transport_sequence.load(Ordering::Acquire);
            if presentation_sequence & 1 != 0 {
                continue;
            }

            let active = self.active_slot.load(Ordering::Acquire);
            let slot = &self.slots[active];
            let sequence = slot.sequence.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                continue;
            }

            let mut candidate = WaveformView {
                snapshot_revision: slot.revision.load(Ordering::Relaxed),
                snapshot_mode: SnapshotMode::from_raw(slot.mode.load(Ordering::Relaxed)),
                snapshot_window: WindowLength::from_raw(slot.window_raw.load(Ordering::Relaxed)),
                snapshot_tempo_bpm: f32_from_bits_or_none(slot.tempo_bits.load(Ordering::Relaxed)),
                snapshot_beat_index: (slot.beat_index_valid.load(Ordering::Relaxed) != 0)
                    .then(|| slot.beat_index.load(Ordering::Relaxed) as i64),
                sample_count: slot.sample_count.load(Ordering::Relaxed) as usize,
                envelope: [EnvelopePoint::default(); ENVELOPE_BINS],
                live_revision: 0,
                live_valid: false,
                live_mode: SnapshotMode::Empty,
                live_window: WindowLength::DEFAULT,
                live_tempo_bpm: None,
                live_beat_index: None,
                live_sample_count: 0,
                target_sample_count: 0,
                live_envelope: [EnvelopePoint::default(); ENVELOPE_BINS],
                display_revision: 0,
                display_valid: false,
                display_mode: SnapshotMode::Empty,
                display_window: WindowLength::DEFAULT,
                display_tempo_bpm: None,
                display_beat_index: None,
                display_sample_count: 0,
                display_target_sample_count: 0,
                display_envelope: [EnvelopePoint::default(); ENVELOPE_BINS],
                ..*view
            };
            for bin in 0..ENVELOPE_BINS {
                candidate.envelope[bin] = EnvelopePoint {
                    min: f32::from_bits(slot.mins[bin].load(Ordering::Relaxed)),
                    max: f32::from_bits(slot.maxs[bin].load(Ordering::Relaxed)),
                };
            }

            let end_sequence = slot.sequence.load(Ordering::Acquire);
            if sequence != end_sequence
                || end_sequence & 1 != 0
                || self.active_slot.load(Ordering::Acquire) != active
            {
                continue;
            }

            let live_active = self.live_active_slot.load(Ordering::Acquire);
            let live_slot = &self.live_slots[live_active];
            let live_sequence = live_slot.sequence.load(Ordering::Acquire);
            if live_sequence & 1 != 0 {
                continue;
            }
            let slot_valid = live_slot.valid.load(Ordering::Relaxed) != 0;
            candidate.display_valid = slot_valid;
            if slot_valid {
                candidate.display_revision = live_slot.revision.load(Ordering::Relaxed);
                candidate.display_mode =
                    SnapshotMode::from_raw(live_slot.mode.load(Ordering::Relaxed));
                candidate.display_window =
                    WindowLength::from_raw(live_slot.window_raw.load(Ordering::Relaxed));
                candidate.display_tempo_bpm =
                    f32_from_bits_or_none(live_slot.tempo_bits.load(Ordering::Relaxed));
                candidate.display_beat_index = (live_slot.beat_index_valid.load(Ordering::Relaxed)
                    != 0)
                    .then(|| live_slot.beat_index.load(Ordering::Relaxed) as i64);
                candidate.display_sample_count =
                    live_slot.sample_count.load(Ordering::Relaxed) as usize;
                candidate.display_target_sample_count =
                    live_slot.target_sample_count.load(Ordering::Relaxed) as usize;
                for bin in 0..ENVELOPE_BINS {
                    candidate.display_envelope[bin] = EnvelopePoint {
                        min: f32::from_bits(live_slot.mins[bin].load(Ordering::Relaxed)),
                        max: f32::from_bits(live_slot.maxs[bin].load(Ordering::Relaxed)),
                    };
                }
            }
            candidate.live_valid =
                self.live_active_valid.load(Ordering::Relaxed) != 0 && slot_valid;
            if candidate.live_valid {
                candidate.live_revision = candidate.display_revision;
                candidate.live_mode = candidate.display_mode;
                candidate.live_window = candidate.display_window;
                candidate.live_tempo_bpm = candidate.display_tempo_bpm;
                candidate.live_beat_index = candidate.display_beat_index;
                candidate.live_sample_count = candidate.display_sample_count;
                candidate.target_sample_count = candidate.display_target_sample_count;
                candidate.live_envelope = candidate.display_envelope;
            }

            let end_live_sequence = live_slot.sequence.load(Ordering::Acquire);
            if live_sequence != end_live_sequence
                || end_live_sequence & 1 != 0
                || self.live_active_slot.load(Ordering::Acquire) != live_active
            {
                continue;
            }

            let current_tempo_bpm =
                f32_from_bits_or_none(self.current_tempo_bits.load(Ordering::Acquire));
            let current_song_pos_beats = (self.current_position_valid.load(Ordering::Acquire) != 0)
                .then(|| f64::from_bits(self.current_position_bits.load(Ordering::Relaxed)));
            let is_playing = self.is_playing.load(Ordering::Acquire) != 0;
            let timeline_available = self.timeline_available.load(Ordering::Acquire) != 0;
            let end_presentation_sequence = self.transport_sequence.load(Ordering::Acquire);
            if presentation_sequence != end_presentation_sequence
                || end_presentation_sequence & 1 != 0
            {
                continue;
            }

            candidate.current_tempo_bpm = current_tempo_bpm;
            candidate.current_song_pos_beats = current_song_pos_beats;
            candidate.is_playing = is_playing;
            candidate.timeline_available = timeline_available;
            candidate.redraw_revision = self.redraw_revision.load(Ordering::Acquire);
            *view = candidate;
            return true;
        }
        false
    }

    /// Return the latest redraw revision without copying the envelope.
    pub fn redraw_revision(&self) -> u64 {
        self.redraw_revision.load(Ordering::Acquire)
    }
}

impl PublicationTransaction<'_> {
    fn publish_presentation(
        &self,
        transport: Option<TransportInfo>,
        live: Option<LivePreview<'_>>,
    ) {
        let publication = self.publication;
        if let Some(transport) = transport {
            let tempo = valid_tempo(transport.tempo_bpm);
            let position = valid_position(transport.song_pos_beats);
            publication.current_tempo_bits.store(
                tempo.map(|value| value as f32).unwrap_or(0.0).to_bits(),
                Ordering::Relaxed,
            );
            if let Some(position) = position {
                publication
                    .current_position_bits
                    .store(position.to_bits(), Ordering::Relaxed);
                publication
                    .current_position_valid
                    .store(1, Ordering::Relaxed);
            } else {
                publication
                    .current_position_bits
                    .store(0, Ordering::Relaxed);
                publication
                    .current_position_valid
                    .store(0, Ordering::Relaxed);
            }
            publication
                .is_playing
                .store(u32::from(transport.is_playing), Ordering::Relaxed);
            publication
                .timeline_available
                .store(u32::from(position.is_some()), Ordering::Relaxed);
        }

        if let Some(live) = live {
            if !live.valid || live.sample_count == 0 || live.target_sample_count == 0 {
                // Keep the last valid slot as a display-only snapshot. The
                // separate validity bit invalidates only the current capture,
                // so a stop/seek/reset cannot blank a prefix before the next
                // complete window is available.
                publication.live_active_valid.store(0, Ordering::Relaxed);
            } else {
                let active = publication.live_active_slot.load(Ordering::Acquire);
                let inactive = active ^ 1;
                let slot = &publication.live_slots[inactive];
                let slot_sequence = slot.sequence.load(Ordering::Relaxed);
                slot.sequence.store(slot_sequence | 1, Ordering::Relaxed);

                let revision = publication.live_revision.fetch_add(1, Ordering::Relaxed) + 1;
                slot.revision.store(revision, Ordering::Relaxed);
                let target_sample_count = live.target_sample_count.min(u32::MAX as usize);
                let sample_count = live
                    .sample_count
                    .min(target_sample_count)
                    .min(u32::MAX as usize);
                slot.valid.store(1, Ordering::Relaxed);
                slot.mode.store(live.mode.as_raw(), Ordering::Relaxed);
                slot.window_raw
                    .store(live.window.as_raw(), Ordering::Relaxed);
                slot.tempo_bits.store(
                    valid_tempo(live.tempo_bpm)
                        .map(|value| (value as f32).to_bits())
                        .unwrap_or(0),
                    Ordering::Relaxed,
                );
                if let Some(index) = live.beat_index {
                    slot.beat_index.store(index as u64, Ordering::Relaxed);
                    slot.beat_index_valid.store(1, Ordering::Relaxed);
                } else {
                    slot.beat_index_valid.store(0, Ordering::Relaxed);
                }
                slot.sample_count
                    .store(sample_count as u32, Ordering::Relaxed);
                slot.target_sample_count
                    .store(target_sample_count as u32, Ordering::Relaxed);
                if let Some(envelope) = live.envelope {
                    for (bin, point) in envelope.iter().enumerate() {
                        let point = if point.min <= point.max {
                            *point
                        } else {
                            EnvelopePoint::default()
                        };
                        slot.mins[bin].store(point.min.to_bits(), Ordering::Relaxed);
                        slot.maxs[bin].store(point.max.to_bits(), Ordering::Relaxed);
                    }
                }
                slot.sequence
                    .store((slot_sequence | 1).wrapping_add(1), Ordering::Release);
                publication
                    .live_active_slot
                    .store(inactive, Ordering::Release);
                publication.live_active_valid.store(1, Ordering::Relaxed);
            }
        }

        publication.redraw_revision.fetch_add(1, Ordering::Release);
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_live_preview(
        &self,
        transport: TransportInfo,
        mode: SnapshotMode,
        window: WindowLength,
        tempo_bpm: Option<f64>,
        beat_index: Option<i64>,
        sample_count: usize,
        target_sample_count: usize,
        envelope: &[EnvelopePoint; ENVELOPE_BINS],
    ) {
        self.publish_presentation(
            Some(transport),
            Some(LivePreview {
                valid: true,
                mode,
                window,
                tempo_bpm,
                beat_index,
                sample_count,
                target_sample_count,
                envelope: Some(envelope),
            }),
        );
    }

    fn clear_live_preview_with_transport(&self, transport: TransportInfo) {
        self.publish_presentation(Some(transport), Some(LivePreview::clear()));
    }

    fn publish_envelope(
        &self,
        mode: SnapshotMode,
        window: WindowLength,
        tempo_bpm: Option<f64>,
        beat_index: Option<i64>,
        sample_count: usize,
        envelope: &[EnvelopePoint; ENVELOPE_BINS],
    ) {
        let publication = self.publication;
        let sample_count = sample_count.min(u32::MAX as usize);
        let active = publication.active_slot.load(Ordering::Acquire);
        let inactive = active ^ 1;
        let slot = &publication.slots[inactive];
        let sequence = slot.sequence.load(Ordering::Relaxed);
        slot.sequence.store(sequence | 1, Ordering::Relaxed);

        let revision = publication
            .snapshot_revision
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        slot.revision.store(revision, Ordering::Relaxed);
        slot.mode.store(mode.as_raw(), Ordering::Relaxed);
        slot.window_raw.store(window.as_raw(), Ordering::Relaxed);
        slot.tempo_bits.store(
            valid_tempo(tempo_bpm)
                .map(|value| (value as f32).to_bits())
                .unwrap_or(0),
            Ordering::Relaxed,
        );
        if let Some(index) = beat_index {
            slot.beat_index.store(index as u64, Ordering::Relaxed);
            slot.beat_index_valid.store(1, Ordering::Relaxed);
        } else {
            slot.beat_index_valid.store(0, Ordering::Relaxed);
        }
        slot.sample_count
            .store(sample_count as u32, Ordering::Relaxed);
        for (bin, point) in envelope.iter().enumerate() {
            let point = if point.min <= point.max {
                *point
            } else {
                EnvelopePoint::default()
            };
            slot.mins[bin].store(point.min.to_bits(), Ordering::Relaxed);
            slot.maxs[bin].store(point.max.to_bits(), Ordering::Relaxed);
        }
        slot.sequence
            .store((sequence | 1).wrapping_add(1), Ordering::Release);
        publication.active_slot.store(inactive, Ordering::Release);
    }

    fn commit(self) {
        self.publication
            .transport_sequence
            .store(self.sequence.wrapping_add(1), Ordering::Release);
    }
}

fn f32_from_bits_or_none(bits: u32) -> Option<f32> {
    (bits != 0)
        .then(|| f32::from_bits(bits))
        .filter(|value| value.is_finite())
}

/// Return a host tempo only when it is finite and inside the bounded capture range.
pub fn valid_tempo(tempo_bpm: Option<f64>) -> Option<f64> {
    tempo_bpm.filter(|tempo| tempo.is_finite() && (MIN_TEMPO_BPM..=MAX_TEMPO_BPM).contains(tempo))
}

fn valid_position(position: Option<f64>) -> Option<f64> {
    position.filter(|value| value.is_finite())
}

/// Calculate a bounded duration in samples for a selectable beat window.
pub fn window_samples(
    sample_rate: f64,
    tempo_bpm: f64,
    window: WindowLength,
    capacity: usize,
) -> Option<usize> {
    if window == WindowLength::OneBeat {
        return quarter_window_samples(sample_rate, tempo_bpm, capacity);
    }
    if !sample_rate.is_finite()
        || sample_rate <= 0.0
        || valid_tempo(Some(tempo_bpm)).is_none()
        || capacity == 0
    {
        return None;
    }
    let beats = f64::from(window.beats());
    let samples = (sample_rate / tempo_bpm) * 60.0 * beats;
    checked_capacity_samples(samples, capacity)
}

/// Calculate a one-beat duration in samples, bounded by the capture capacity.
pub fn quarter_window_samples(sample_rate: f64, tempo_bpm: f64, capacity: usize) -> Option<usize> {
    if !sample_rate.is_finite()
        || sample_rate <= 0.0
        || valid_tempo(Some(tempo_bpm)).is_none()
        || capacity == 0
    {
        return None;
    }
    checked_capacity_samples((sample_rate / tempo_bpm) * 60.0, capacity)
}

/// Calculate the bounded 500 ms fallback window in samples.
pub fn fallback_window_samples(sample_rate: f64, capacity: usize) -> usize {
    if !sample_rate.is_finite() || sample_rate <= 0.0 || capacity == 0 {
        return 1;
    }
    let samples = (sample_rate * FALLBACK_WINDOW_SECONDS).ceil().max(1.0);
    checked_capacity_samples(samples, capacity).unwrap_or(capacity)
}

fn checked_capacity_samples(samples: f64, capacity: usize) -> Option<usize> {
    if !samples.is_finite() || samples <= 0.0 {
        return None;
    }
    let rounded = samples.ceil();
    if !rounded.is_finite() || rounded >= usize::MAX as f64 || rounded > capacity as f64 {
        return None;
    }
    let samples = rounded as usize;
    (samples > 0 && samples <= capacity).then_some(samples)
}

/// Map a quarter-note offset to a logical x coordinate.
pub fn grid_x(beat_offset: f64, window_beats: f64, width: f32) -> f32 {
    if !beat_offset.is_finite() || !window_beats.is_finite() || window_beats <= 0.0 {
        return 0.0;
    }
    (beat_offset / window_beats * f64::from(width.max(0.0))).clamp(0.0, f64::from(width.max(0.0)))
        as f32
}

/// Return the aligned integer beat at the start of the current window.
pub fn aligned_window_start(position: f64, window: WindowLength) -> Option<i64> {
    if !position.is_finite() {
        return None;
    }
    let width = f64::from(window.beats());
    let window_index = (position / width).floor();
    if !window_index.is_finite()
        || window_index < i64::MIN as f64
        || window_index >= 9_223_372_036_854_775_808.0
        || window_index.fract() != 0.0
    {
        return None;
    }
    (window_index as i64).checked_mul(i64::from(window.beats()))
}

/// Return the fractional current position within a selected beat window.
pub fn window_phase(song_pos_beats: Option<f64>, window: WindowLength) -> Option<f32> {
    let position = valid_position(song_pos_beats)?;
    let width = f64::from(window.beats());
    Some((position.rem_euclid(width) / width).clamp(0.0, 0.999_999) as f32)
}

/// Return the fractional current position in a one-beat waveform window.
pub fn current_phase(song_pos_beats: Option<f64>) -> Option<f32> {
    window_phase(song_pos_beats, WindowLength::OneBeat)
}

/// Fixed-capacity audio-side capture state.
pub struct CaptureEngine {
    sample_rate: f64,
    capacity_samples: usize,
    active_window: WindowLength,
    capture_mode: Option<SnapshotMode>,
    synced_interval: Option<i64>,
    synced_len: usize,
    synced_envelope: [EnvelopePoint; ENVELOPE_BINS],
    rolling_target: usize,
    rolling_len: usize,
    rolling_envelope: [EnvelopePoint; ENVELOPE_BINS],
    live_next_publish_sample_count: usize,
    last_position: Option<f64>,
    last_tempo: Option<f64>,
    last_playing: bool,
}

impl CaptureEngine {
    /// Reserve the logical capture capacity during plugin activation.
    pub fn new(sample_rate: f64) -> Self {
        Self::with_capacity(sample_rate, MAX_CAPTURE_SAMPLES)
    }

    /// Construct a smaller fixed-capacity engine for focused tests.
    pub fn with_capacity(sample_rate: f64, capacity: usize) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            capacity_samples: capacity.max(1),
            active_window: WindowLength::DEFAULT,
            capture_mode: None,
            synced_interval: None,
            synced_len: 0,
            synced_envelope: [EMPTY_CAPTURE_ENVELOPE; ENVELOPE_BINS],
            rolling_target: 0,
            rolling_len: 0,
            rolling_envelope: [EMPTY_CAPTURE_ENVELOPE; ENVELOPE_BINS],
            live_next_publish_sample_count: 0,
            last_position: None,
            last_tempo: None,
            last_playing: false,
        }
    }

    /// Reset capture and transport continuity without reallocating.
    #[cfg(feature = "vst3")]
    pub fn reset(&mut self, publication: &WaveformPublication) {
        self.reset_partial();
        self.last_position = None;
        self.last_tempo = None;
        self.last_playing = false;
        publication.clear_live_preview();
    }

    /// Process one bounded stereo source block and publish only complete windows.
    pub fn process_block(
        &mut self,
        left: &[f32],
        right: &[f32],
        transport: TransportInfo,
        publication: &WaveformPublication,
    ) {
        // Keep the outer view sequence odd for this entire bounded block.
        let transaction = publication.begin_publication();
        let frames = left.len().min(right.len());
        let selected_window = publication.selected_window();
        if selected_window != self.active_window {
            self.active_window = selected_window;
            self.reset_partial();
        }
        let tempo = valid_tempo(transport.tempo_bpm);
        let position = valid_position(transport.song_pos_beats);
        let mode = if tempo.is_some() && position.is_some() {
            SnapshotMode::Synced
        } else if tempo.is_some() {
            SnapshotMode::UnsyncedTempo
        } else {
            SnapshotMode::UnsyncedFallback
        };
        let presentation_transport = TransportInfo {
            tempo_bpm: tempo,
            song_pos_beats: position,
            is_playing: transport.is_playing,
        };

        if !transport.is_playing || frames == 0 {
            self.reset_partial();
            self.last_position = position;
            self.last_tempo = tempo;
            self.last_playing = false;
            transaction.clear_live_preview_with_transport(presentation_transport);
            transaction.commit();
            return;
        }

        let discontinuity = !self.last_playing
            || self.capture_mode != Some(mode)
            || tempo_changed(self.last_tempo, tempo)
            || (mode == SnapshotMode::Synced && !self.position_is_continuous(position));
        if discontinuity {
            self.reset_partial();
        }

        match mode {
            SnapshotMode::Synced => {
                if let (Some(position), Some(tempo)) = (position, tempo) {
                    self.process_synced(
                        left,
                        right,
                        position,
                        tempo,
                        selected_window,
                        &transaction,
                    );
                }
            }
            SnapshotMode::UnsyncedTempo | SnapshotMode::UnsyncedFallback => {
                self.process_rolling(left, right, tempo, mode, selected_window, &transaction);
            }
            SnapshotMode::Empty => unreachable!("capture mode is never empty while playing"),
        }

        let live_capture = match mode {
            SnapshotMode::Synced => window_samples(
                self.sample_rate,
                tempo.unwrap_or(0.0),
                selected_window,
                self.capacity_samples,
            )
            .and_then(|target| {
                self.synced_interval.map(|beat_index| {
                    (
                        beat_index,
                        self.synced_len,
                        target,
                        selected_window,
                        &self.synced_envelope,
                    )
                })
            }),
            SnapshotMode::UnsyncedTempo | SnapshotMode::UnsyncedFallback => {
                (self.rolling_target > 0 && self.rolling_len > 0).then_some((
                    0,
                    self.rolling_len,
                    self.rolling_target,
                    selected_window,
                    &self.rolling_envelope,
                ))
            }
            SnapshotMode::Empty => None,
        };
        if let Some((beat_index, sample_count, target_sample_count, window, envelope)) =
            live_capture
        {
            let live_is_due = self.live_next_publish_sample_count == 0
                || sample_count >= self.live_next_publish_sample_count;
            if live_is_due {
                transaction.publish_live_preview(
                    presentation_transport,
                    mode,
                    window,
                    tempo,
                    (mode == SnapshotMode::Synced).then_some(beat_index),
                    sample_count,
                    target_sample_count,
                    envelope,
                );
                self.live_next_publish_sample_count = next_live_publish_sample_count(
                    sample_count,
                    live_preview_cadence_samples(self.sample_rate),
                );
                // The live publication carries the transport tuple in this
                // block's outer publication transaction.
            } else {
                transaction.publish_presentation(Some(presentation_transport), None);
            }
        } else {
            transaction.clear_live_preview_with_transport(presentation_transport);
        }

        self.capture_mode = Some(mode);
        self.last_position = position
            .map(|start| start + frames as f64 * tempo.unwrap_or(0.0) / (60.0 * self.sample_rate));
        self.last_tempo = tempo;
        self.last_playing = true;
        transaction.commit();
    }

    fn process_synced(
        &mut self,
        left: &[f32],
        right: &[f32],
        position: f64,
        tempo: f64,
        window: WindowLength,
        transaction: &PublicationTransaction<'_>,
    ) {
        let increment = tempo / (60.0 * self.sample_rate);
        let Some(target) = window_samples(self.sample_rate, tempo, window, self.capacity_samples)
        else {
            self.synced_interval = None;
            self.synced_len = 0;
            self.live_next_publish_sample_count = 0;
            return;
        };

        let mut previous_interval = aligned_window_start(position, window);
        if previous_interval.is_none() {
            self.synced_interval = None;
            self.synced_len = 0;
            self.live_next_publish_sample_count = 0;
            clear_capture_envelope(&mut self.synced_envelope);
            return;
        }
        for (index, (&left, &right)) in left.iter().zip(right).enumerate() {
            let beat = position + index as f64 * increment;
            let Some(interval) = aligned_window_start(beat, window) else {
                self.synced_interval = None;
                self.synced_len = 0;
                self.live_next_publish_sample_count = 0;
                clear_capture_envelope(&mut self.synced_envelope);
                return;
            };
            match self.synced_interval {
                None => {
                    let near_boundary = (beat - interval as f64).abs() <= increment.abs() * 0.5;
                    let crossed_boundary =
                        previous_interval.is_some_and(|previous| previous != interval);
                    if near_boundary || crossed_boundary {
                        self.synced_interval = Some(interval);
                        self.synced_len = 0;
                        self.live_next_publish_sample_count = 0;
                        clear_capture_envelope(&mut self.synced_envelope);
                    }
                }
                Some(current) if interval != current => {
                    let expected_next = current.checked_add(i64::from(window.beats()));
                    if expected_next == Some(interval) && self.synced_len == target {
                        transaction.publish_envelope(
                            SnapshotMode::Synced,
                            window,
                            Some(tempo),
                            Some(current),
                            target,
                            &self.synced_envelope,
                        );
                    }
                    self.synced_interval = Some(interval);
                    self.synced_len = 0;
                    self.live_next_publish_sample_count = 0;
                    clear_capture_envelope(&mut self.synced_envelope);
                }
                Some(_) => {}
            }

            if self.synced_interval == Some(interval) && self.synced_len < target {
                accumulate_capture_envelope(
                    &mut self.synced_envelope,
                    self.synced_len,
                    target,
                    left,
                    right,
                );
                self.synced_len += 1;
            }
            previous_interval = Some(interval);
        }
    }

    fn process_rolling(
        &mut self,
        left: &[f32],
        right: &[f32],
        tempo: Option<f64>,
        mode: SnapshotMode,
        window: WindowLength,
        transaction: &PublicationTransaction<'_>,
    ) {
        let target = tempo
            .and_then(|tempo| {
                window_samples(self.sample_rate, tempo, window, self.capacity_samples)
            })
            .unwrap_or_else(|| fallback_window_samples(self.sample_rate, self.capacity_samples));
        if self.capture_mode != Some(mode) || self.rolling_target != target {
            self.reset_partial();
            self.capture_mode = Some(mode);
            self.rolling_target = target;
        }

        for (&left, &right) in left.iter().zip(right) {
            if self.rolling_len >= self.rolling_target {
                self.rolling_len = 0;
                self.live_next_publish_sample_count = 0;
                clear_capture_envelope(&mut self.rolling_envelope);
            }
            accumulate_capture_envelope(
                &mut self.rolling_envelope,
                self.rolling_len,
                self.rolling_target,
                left,
                right,
            );
            self.rolling_len += 1;
            if self.rolling_len == self.rolling_target {
                transaction.publish_envelope(
                    mode,
                    window,
                    tempo,
                    None,
                    self.rolling_target,
                    &self.rolling_envelope,
                );
            }
        }
    }

    fn position_is_continuous(&self, position: Option<f64>) -> bool {
        let (Some(expected), Some(position), Some(tempo)) =
            (self.last_position, position, self.last_tempo)
        else {
            return false;
        };
        let beat_per_sample = tempo / (60.0 * self.sample_rate);
        let tolerance = (beat_per_sample * POSITION_TOLERANCE_SAMPLES).max(f64::EPSILON);
        (position - expected).abs() <= tolerance
    }

    fn reset_partial(&mut self) {
        self.capture_mode = None;
        self.synced_interval = None;
        self.synced_len = 0;
        clear_capture_envelope(&mut self.synced_envelope);
        self.rolling_target = 0;
        self.rolling_len = 0;
        clear_capture_envelope(&mut self.rolling_envelope);
        self.live_next_publish_sample_count = 0;
    }
}

fn tempo_changed(previous: Option<f64>, current: Option<f64>) -> bool {
    match (previous, current) {
        (Some(previous), Some(current)) => previous != current,
        (None, None) => false,
        _ => true,
    }
}

fn live_preview_cadence_samples(sample_rate: f64) -> usize {
    (sample_rate / LIVE_PREVIEW_HZ).ceil().max(1.0) as usize
}

fn next_live_publish_sample_count(sample_count: usize, cadence: usize) -> usize {
    sample_count.saturating_add(cadence.max(1))
}

fn clear_capture_envelope(envelope: &mut [EnvelopePoint; ENVELOPE_BINS]) {
    envelope.fill(EMPTY_CAPTURE_ENVELOPE);
}

fn accumulate_capture_envelope(
    envelope: &mut [EnvelopePoint; ENVELOPE_BINS],
    sample_index: usize,
    sample_count: usize,
    left: f32,
    right: f32,
) {
    let bin = sample_index
        .saturating_mul(ENVELOPE_BINS)
        .checked_div(sample_count.max(1))
        .unwrap_or(0)
        .min(ENVELOPE_BINS - 1);
    let left = sanitize_sample(left);
    let right = sanitize_sample(right);
    envelope[bin].min = envelope[bin].min.min(left).min(right);
    envelope[bin].max = envelope[bin].max.max(left).max(right);
}

fn sanitize_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport(position: Option<f64>, tempo: Option<f64>, playing: bool) -> TransportInfo {
        TransportInfo {
            tempo_bpm: tempo,
            song_pos_beats: position,
            is_playing: playing,
        }
    }

    fn read(publication: &WaveformPublication) -> WaveformView {
        let mut view = WaveformView::default();
        assert!(publication.read_view(&mut view));
        view
    }

    #[test]
    fn quarter_and_fallback_window_math_is_bounded() {
        assert_eq!(
            quarter_window_samples(48_000.0, 120.0, 100_000),
            Some(24_000)
        );
        assert_eq!(fallback_window_samples(48_000.0, 100_000), 24_000);
        assert_eq!(quarter_window_samples(48_000.0, 10.0, 100), None);
        assert_eq!(fallback_window_samples(48_000.0, 100), 100);
        assert_eq!(
            window_samples(
                192_000.0,
                20.0,
                WindowLength::EightBeats,
                MAX_CAPTURE_SAMPLES,
            ),
            Some(4_608_000)
        );
        assert_eq!(
            window_samples(192_000.0, 20.0, WindowLength::EightBeats, 4_607_999),
            None
        );
        assert_eq!(
            window_samples(192_000.0, 20.0, WindowLength::FourBeats, 2_304_000),
            Some(2_304_000)
        );
    }

    #[test]
    fn window_length_domain_has_stable_labels_and_validated_raw_selection() {
        assert_eq!(WindowLength::DEFAULT, WindowLength::OneBeat);
        assert_eq!(WindowLength::ALL.len(), 4);
        assert_eq!(WindowLength::OneBeat.beats(), 1);
        assert_eq!(WindowLength::TwoBeats.beats(), 2);
        assert_eq!(WindowLength::FourBeats.beats(), 4);
        assert_eq!(WindowLength::EightBeats.beats(), 8);
        let expected_order = [
            WindowLength::OneBeat,
            WindowLength::TwoBeats,
            WindowLength::FourBeats,
            WindowLength::EightBeats,
        ];
        assert_eq!(WindowLength::ALL, expected_order);
        for (window, expected_label) in expected_order.into_iter().zip(["1:4", "1:2", "1:1", "2:1"])
        {
            assert_eq!(window.label(), expected_label);
            assert_eq!(WindowLength::from_raw(window.as_raw()), window);
        }
        assert_eq!(WindowLength::from_raw(u32::MAX), WindowLength::OneBeat);

        let publication = WaveformPublication::new();
        assert_eq!(publication.selected_window(), WindowLength::DEFAULT);
        assert!(!publication.set_selected_window(WindowLength::OneBeat));
        assert_eq!(publication.redraw_revision(), 0);
        assert!(publication.set_selected_window(WindowLength::FourBeats));
        assert_eq!(publication.selected_window(), WindowLength::FourBeats);
        assert_eq!(publication.redraw_revision(), 1);
        assert!(!publication.set_selected_window(WindowLength::FourBeats));
        assert_eq!(publication.redraw_revision(), 1);
        for window in WindowLength::ALL {
            publication.set_selected_window(window);
            assert_eq!(publication.selected_window(), window);
        }
    }

    #[test]
    fn multi_beat_alignment_is_floor_safe_for_negative_positions() {
        assert_eq!(
            aligned_window_start(-8.0, WindowLength::FourBeats),
            Some(-8)
        );
        assert_eq!(
            aligned_window_start(-7.99, WindowLength::FourBeats),
            Some(-8)
        );
        assert_eq!(
            aligned_window_start(-4.0, WindowLength::FourBeats),
            Some(-4)
        );
        assert_eq!(
            aligned_window_start(-0.01, WindowLength::FourBeats),
            Some(-4)
        );
        assert_eq!(aligned_window_start(0.0, WindowLength::FourBeats), Some(0));
        assert_eq!(aligned_window_start(3.99, WindowLength::FourBeats), Some(0));
        assert_eq!(aligned_window_start(4.0, WindowLength::FourBeats), Some(4));
        assert_eq!(aligned_window_start(f64::MIN, WindowLength::OneBeat), None);
        assert_eq!(
            aligned_window_start(i64::MAX as f64, WindowLength::EightBeats),
            None
        );
        assert_eq!(
            window_phase(Some(-0.25), WindowLength::FourBeats),
            Some(0.9375)
        );
    }

    #[test]
    fn grid_math_clamps_and_phase_tracks_the_current_beat() {
        assert_eq!(grid_x(0.0, 1.0, 200.0), 0.0);
        assert_eq!(grid_x(0.5, 1.0, 200.0), 100.0);
        assert_eq!(grid_x(2.0, 1.0, 200.0), 200.0);
        assert_eq!(current_phase(Some(3.25)), Some(0.25));
        assert_eq!(current_phase(Some(-0.25)), Some(0.75));
        assert_eq!(current_phase(None), None);
    }

    #[test]
    fn publication_exposes_a_coherent_min_max_envelope() {
        let publication = WaveformPublication::new();
        let mut envelope = [EnvelopePoint::default(); ENVELOPE_BINS];
        envelope[0] = EnvelopePoint {
            min: -1.0,
            max: 0.25,
        };
        envelope[1] = EnvelopePoint {
            min: 0.5,
            max: 0.75,
        };
        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::DEFAULT,
            Some(120.0),
            Some(4),
            4,
            &envelope,
        );
        let view = read(&publication);
        assert_eq!(view.snapshot_revision, 1);
        assert_eq!(view.snapshot_mode, SnapshotMode::Synced);
        assert_eq!(view.snapshot_beat_index, Some(4));
        assert_eq!(view.sample_count, 4);
        assert!(view.envelope.iter().any(|point| point.min < 0.0));
        assert!(view.envelope.iter().any(|point| point.max > 0.7));
    }

    #[test]
    fn publication_keeps_window_metadata_with_each_lane() {
        let publication = WaveformPublication::new();
        let envelope = [EnvelopePoint {
            min: -0.5,
            max: 0.75,
        }; ENVELOPE_BINS];
        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::FourBeats,
            Some(120.0),
            Some(4),
            8,
            &envelope,
        );
        publication.publish_live_preview(
            transport(Some(8.25), Some(120.0), true),
            SnapshotMode::Synced,
            WindowLength::EightBeats,
            Some(120.0),
            Some(8),
            4,
            16,
            &envelope,
        );

        let view = read(&publication);
        assert_eq!(view.snapshot_window, WindowLength::FourBeats);
        assert_eq!(view.live_window, WindowLength::EightBeats);
    }

    #[test]
    fn outer_publication_sequence_rejects_completed_mutation_until_commit() {
        let publication = WaveformPublication::new();
        let generation_a_envelope = [EnvelopePoint {
            min: -0.25,
            max: 0.25,
        }; ENVELOPE_BINS];
        let generation_b_envelope = [EnvelopePoint {
            min: -0.9,
            max: 0.8,
        }; ENVELOPE_BINS];

        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::OneBeat,
            Some(60.0),
            Some(0),
            8,
            &generation_a_envelope,
        );
        publication.update_transport(transport(Some(0.0), Some(60.0), false));
        let generation_a = read(&publication);

        let transaction = publication.begin_publication();
        transaction.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::FourBeats,
            Some(120.0),
            Some(8),
            32,
            &generation_b_envelope,
        );
        assert_eq!(
            publication.transport_sequence.load(Ordering::Acquire) & 1,
            1
        );

        let mut retained = generation_a;
        assert!(!publication.read_view(&mut retained));
        assert_eq!(retained, generation_a);

        transaction.publish_presentation(Some(transport(Some(8.0), Some(120.0), true)), None);
        transaction.commit();
        assert_eq!(
            publication.transport_sequence.load(Ordering::Acquire) & 1,
            0
        );

        let generation_b = read(&publication);
        assert_eq!(generation_b.snapshot_revision, 2);
        assert_eq!(generation_b.snapshot_mode, SnapshotMode::Synced);
        assert_eq!(generation_b.snapshot_window, WindowLength::FourBeats);
        assert_eq!(generation_b.snapshot_tempo_bpm, Some(120.0));
        assert_eq!(generation_b.snapshot_beat_index, Some(8));
        assert_eq!(generation_b.sample_count, 32);
        assert_eq!(generation_b.envelope, generation_b_envelope);
        assert_eq!(generation_b.current_tempo_bpm, Some(120.0));
        assert_eq!(generation_b.current_song_pos_beats, Some(8.0));
        assert!(generation_b.is_playing);
    }

    #[test]
    fn live_preview_is_visible_before_a_completed_frame() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(60.0, 120);
        engine.process_block(
            &[0.25; 4],
            &[0.75; 4],
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );

        let view = read(&publication);
        assert_eq!(view.snapshot_revision, 0);
        assert_eq!(view.live_revision, 1);
        assert!(view.live_valid);
        assert_eq!(view.live_mode, SnapshotMode::Synced);
        assert_eq!(view.live_beat_index, Some(0));
        assert_eq!(view.live_sample_count, 4);
        assert_eq!(view.target_sample_count, 60);
        assert_eq!(view.live_envelope[0].min, 0.25);
        assert_eq!(view.live_envelope[0].max, 0.75);
    }

    #[test]
    fn live_preview_updates_when_amplitude_changes() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(60.0, 120);
        engine.process_block(
            &[0.1],
            &[0.1],
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        let first = read(&publication);

        engine.process_block(
            &[0.9],
            &[0.9],
            transport(Some(1.0 / 60.0), Some(60.0), true),
            &publication,
        );
        let second = read(&publication);

        assert!(second.live_revision > first.live_revision);
        assert!(second.live_envelope.iter().any(|point| point.max > 0.8));
        assert_ne!(second.live_envelope, first.live_envelope);
        assert_eq!(second.snapshot_revision, first.snapshot_revision);
    }

    #[test]
    fn live_preview_publishes_at_most_once_per_block() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(120.0, 240);
        let block = [0.5; 8];
        engine.process_block(
            &block,
            &block,
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        assert_eq!(read(&publication).live_revision, 1);

        engine.process_block(
            &block,
            &block,
            transport(Some(8.0 / 120.0), Some(60.0), true),
            &publication,
        );
        assert_eq!(read(&publication).live_revision, 2);
    }

    #[test]
    fn live_preview_waits_for_a_full_cadence_after_immediate_publication() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(120.0, 240);
        let initial_block = [0.5; 3];
        let sub_cadence_block = [0.5; 1];

        engine.process_block(
            &initial_block,
            &initial_block,
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        assert_eq!(read(&publication).live_revision, 1);

        engine.process_block(
            &sub_cadence_block,
            &sub_cadence_block,
            transport(Some(3.0 / 120.0), Some(60.0), true),
            &publication,
        );
        assert_eq!(read(&publication).live_revision, 1);

        engine.process_block(
            &sub_cadence_block,
            &sub_cadence_block,
            transport(Some(4.0 / 120.0), Some(60.0), true),
            &publication,
        );
        assert_eq!(read(&publication).live_revision, 2);
    }

    #[test]
    fn completed_and_live_revisions_are_independent() {
        let publication = WaveformPublication::new();
        let envelope = [EnvelopePoint {
            min: -0.25,
            max: 0.5,
        }; ENVELOPE_BINS];
        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::DEFAULT,
            Some(60.0),
            Some(3),
            60,
            &envelope,
        );
        let completed = read(&publication);
        assert_eq!(completed.snapshot_revision, 1);
        assert_eq!(completed.live_revision, 0);

        publication.publish_live_preview(
            transport(Some(3.25), Some(60.0), true),
            SnapshotMode::Synced,
            WindowLength::DEFAULT,
            Some(60.0),
            Some(3),
            12,
            60,
            &envelope,
        );
        let first_live = read(&publication);
        assert_eq!(first_live.snapshot_revision, 1);
        assert_eq!(first_live.live_revision, 1);

        publication.publish_live_preview(
            transport(Some(3.5), Some(60.0), true),
            SnapshotMode::Synced,
            WindowLength::DEFAULT,
            Some(60.0),
            Some(3),
            24,
            60,
            &envelope,
        );
        let second_live = read(&publication);
        assert_eq!(second_live.snapshot_revision, 1);
        assert_eq!(second_live.live_revision, 2);
    }

    #[test]
    fn stop_and_discontinuity_retain_live_prefix_without_publishing_completion() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(60.0, 120);
        engine.process_block(
            &[0.5; 2],
            &[0.5; 2],
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        assert!(read(&publication).live_valid);

        engine.process_block(
            &[],
            &[],
            transport(Some(2.0 / 60.0), Some(60.0), false),
            &publication,
        );
        let stopped = read(&publication);
        assert!(!stopped.live_valid);
        assert_eq!(stopped.snapshot_revision, 0);
        assert!(stopped.display_valid);
        assert_eq!(stopped.display_sample_count, 2);
        assert_eq!(stopped.display_envelope[0].min, 0.5);
        assert_eq!(stopped.display_envelope[0].max, 0.5);

        engine.process_block(
            &[0.5],
            &[0.5],
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        assert!(read(&publication).live_valid);
        engine.process_block(
            &[0.5; 2],
            &[0.5; 2],
            transport(Some(3.25), Some(60.0), true),
            &publication,
        );
        let discontinuity = read(&publication);
        assert!(!discontinuity.live_valid);
        assert_eq!(discontinuity.snapshot_revision, 0);
        assert!(discontinuity.display_valid);
        assert_eq!(discontinuity.display_sample_count, 1);
        assert_eq!(discontinuity.display_envelope[0].min, 0.5);
        assert_eq!(discontinuity.display_envelope[0].max, 0.5);
    }

    #[test]
    fn clearing_live_capture_retains_the_last_completed_envelope_for_display() {
        let publication = WaveformPublication::new();
        let envelope = [EnvelopePoint {
            min: -0.75,
            max: 0.9,
        }; ENVELOPE_BINS];
        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::TwoBeats,
            Some(60.0),
            Some(0),
            120,
            &envelope,
        );
        let completed = read(&publication);
        publication.publish_live_preview(
            transport(Some(1.25), Some(60.0), true),
            SnapshotMode::Synced,
            WindowLength::TwoBeats,
            Some(60.0),
            Some(0),
            20,
            120,
            &envelope,
        );
        assert!(read(&publication).live_valid);
        publication.clear_live_preview_with_transport(transport(Some(4.0), Some(60.0), false));
        let cleared = read(&publication);
        assert!(!cleared.live_valid);
        assert_eq!(cleared.snapshot_revision, completed.snapshot_revision);
        assert_eq!(cleared.snapshot_window, WindowLength::TwoBeats);
        assert_eq!(cleared.envelope, completed.envelope);
        assert!(cleared.display_valid);
        assert_eq!(cleared.display_sample_count, 20);
        assert_eq!(cleared.display_envelope, completed.envelope);
    }

    #[test]
    fn selection_change_resets_partial_and_live_but_retains_completed_snapshot() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(60.0, 512);
        let block = [0.25; 60];
        engine.process_block(
            &block,
            &block,
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        engine.process_block(
            &[0.25],
            &[0.25],
            transport(Some(1.0), Some(60.0), true),
            &publication,
        );
        let completed = read(&publication);
        assert_eq!(completed.snapshot_revision, 1);
        assert_eq!(completed.snapshot_window, WindowLength::OneBeat);

        publication.set_selected_window(WindowLength::TwoBeats);
        engine.process_block(
            &[0.9; 4],
            &[0.9; 4],
            transport(Some(1.0), Some(60.0), true),
            &publication,
        );
        let after_change = read(&publication);
        assert_eq!(after_change.snapshot_revision, completed.snapshot_revision);
        assert_eq!(after_change.snapshot_window, WindowLength::OneBeat);
        assert_eq!(after_change.envelope, completed.envelope);
        assert!(!after_change.live_valid);
        assert!(after_change.display_valid);
        assert_eq!(after_change.display_window, WindowLength::OneBeat);
        assert_eq!(after_change.display_sample_count, 1);
        assert_eq!(after_change.display_target_sample_count, 60);
        assert_eq!(after_change.display_envelope[0].min, 0.25);
        assert_eq!(after_change.display_envelope[0].max, 0.25);
    }

    #[test]
    fn selection_reset_retains_visible_partial_for_display_without_completed_snapshot() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(60.0, 512);
        engine.process_block(
            &[0.25; 4],
            &[0.75; 4],
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        let before_change = read(&publication);
        assert_eq!(before_change.snapshot_revision, 0);
        assert!(before_change.live_valid);

        publication.set_selected_window(WindowLength::TwoBeats);
        engine.process_block(
            &[0.9; 4],
            &[0.9; 4],
            transport(Some(0.25), Some(60.0), true),
            &publication,
        );

        let after_change = read(&publication);
        assert_eq!(after_change.snapshot_revision, 0);
        assert!(!after_change.live_valid);
        assert!(after_change.display_valid);
        assert_eq!(after_change.display_window, WindowLength::OneBeat);
        assert_eq!(after_change.display_sample_count, 4);
        assert_eq!(after_change.display_envelope[0].min, 0.25);
        assert_eq!(after_change.display_envelope[0].max, 0.75);
    }

    #[test]
    fn tempo_only_rolling_capture_uses_the_selected_multi_beat_length() {
        let publication = WaveformPublication::new();
        publication.set_selected_window(WindowLength::FourBeats);
        let mut engine = CaptureEngine::with_capacity(4.0, 64);
        let target = window_samples(4.0, 60.0, WindowLength::FourBeats, 64).unwrap();
        let first = vec![0.5; target - 1];
        engine.process_block(
            &first,
            &first,
            transport(None, Some(60.0), true),
            &publication,
        );
        let live = read(&publication);
        assert_eq!(live.snapshot_revision, 0);
        assert!(live.live_valid);
        assert_eq!(live.live_window, WindowLength::FourBeats);
        assert_eq!(live.target_sample_count, target);

        engine.process_block(
            &[0.5],
            &[0.5],
            transport(None, Some(60.0), true),
            &publication,
        );
        let completed = read(&publication);
        assert_eq!(completed.snapshot_revision, 1);
        assert_eq!(completed.snapshot_mode, SnapshotMode::UnsyncedTempo);
        assert_eq!(completed.snapshot_window, WindowLength::FourBeats);
        assert_eq!(completed.sample_count, target);
    }

    #[test]
    fn fallback_window_stays_500_ms_and_live_cadence_remains_bounded() {
        let publication = WaveformPublication::new();
        publication.set_selected_window(WindowLength::EightBeats);
        let mut engine = CaptureEngine::with_capacity(120.0, 120);
        let block = [0.5; 3];
        engine.process_block(&block, &block, transport(None, None, true), &publication);
        let live = read(&publication);
        assert!(live.live_valid);
        assert_eq!(live.live_window, WindowLength::EightBeats);
        assert_eq!(live.target_sample_count, 60);

        engine.process_block(
            &[0.5; 57],
            &[0.5; 57],
            transport(None, None, true),
            &publication,
        );
        let completed = read(&publication);
        assert_eq!(completed.snapshot_revision, 1);
        assert_eq!(completed.snapshot_mode, SnapshotMode::UnsyncedFallback);
        assert_eq!(completed.sample_count, 60);
        assert_eq!(completed.snapshot_window, WindowLength::EightBeats);
    }

    #[test]
    fn live_reader_retries_when_the_active_slot_is_in_progress() {
        let publication = WaveformPublication::new();
        let envelope = [EnvelopePoint {
            min: -0.5,
            max: 0.75,
        }; ENVELOPE_BINS];
        publication.publish_live_preview(
            transport(Some(0.0), Some(120.0), true),
            SnapshotMode::Synced,
            WindowLength::DEFAULT,
            Some(120.0),
            Some(0),
            8,
            24,
            &envelope,
        );
        let previous = read(&publication);
        let active = publication.live_active_slot.load(Ordering::Acquire);
        let sequence = publication.live_slots[active]
            .sequence
            .load(Ordering::Relaxed);
        publication.live_slots[active]
            .sequence
            .store(sequence | 1, Ordering::Relaxed);

        let mut retained = previous;
        assert!(!publication.read_view(&mut retained));
        assert_eq!(retained, previous);

        publication.live_slots[active]
            .sequence
            .store(sequence, Ordering::Release);
        assert!(publication.read_view(&mut retained));
    }

    #[test]
    fn high_resolution_accumulation_preserves_a_narrow_peak() {
        let mut envelope = [EMPTY_CAPTURE_ENVELOPE; ENVELOPE_BINS];

        accumulate_capture_envelope(&mut envelope, 600, ENVELOPE_BINS, 1.0, 1.0);

        assert_eq!(envelope[600].min, 1.0);
        assert_eq!(envelope[600].max, 1.0);
        assert_eq!(envelope[599], EMPTY_CAPTURE_ENVELOPE);
    }

    fn fallback_view(left: &[f32], right: &[f32]) -> WaveformView {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(4.0, 2);
        engine.process_block(left, right, transport(None, None, true), &publication);
        read(&publication)
    }

    #[test]
    fn anti_phase_full_scale_transient_preserves_both_channel_extrema() {
        let view = fallback_view(&[0.0, 1.0], &[0.0, -1.0]);
        let point = view.envelope[ENVELOPE_BINS / 2];

        assert_eq!(view.snapshot_revision, 1);
        assert_eq!(point.min, -1.0);
        assert_eq!(point.max, 1.0);
    }

    #[test]
    fn hard_panned_impulse_preserves_the_source_channel_peak() {
        let view = fallback_view(&[0.0, 1.0], &[0.0, 0.0]);
        let point = view.envelope[ENVELOPE_BINS / 2];

        assert_eq!(view.snapshot_revision, 1);
        assert_eq!(point.min, 0.0);
        assert_eq!(point.max, 1.0);
    }

    #[test]
    fn transport_publication_keeps_validity_and_state_as_one_snapshot() {
        let publication = WaveformPublication::new();
        publication.update_transport(transport(Some(4.0), Some(120.0), true));
        let first = read(&publication);
        assert_eq!(first.current_song_pos_beats, Some(4.0));
        assert!(first.timeline_available);
        assert!(first.is_playing);

        let transaction = publication.begin_publication();
        let mut interrupted = first;
        assert!(!publication.read_view(&mut interrupted));
        assert_eq!(interrupted, first);
        transaction.commit();

        publication.update_transport(transport(None, None, false));
        let second = read(&publication);
        assert_eq!(second.current_song_pos_beats, None);
        assert_eq!(second.current_tempo_bpm, None);
        assert!(!second.timeline_available);
        assert!(!second.is_playing);
    }

    #[test]
    fn synced_capture_publishes_only_a_complete_quarter() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(8.0, 64);
        let left = [0.25; 4];
        let right = [0.75; 4];
        engine.process_block(
            &left,
            &right,
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        assert_eq!(read(&publication).snapshot_revision, 0);
        engine.process_block(
            &left,
            &right,
            transport(Some(0.5), Some(60.0), true),
            &publication,
        );
        assert_eq!(read(&publication).snapshot_revision, 0);
        engine.process_block(
            &left,
            &right,
            transport(Some(1.0), Some(60.0), true),
            &publication,
        );
        let view = read(&publication);
        assert_eq!(view.snapshot_revision, 1);
        assert_eq!(view.snapshot_beat_index, Some(0));
        assert_eq!(view.sample_count, 8);
        assert_eq!(view.snapshot_tempo_bpm, Some(60.0));
        assert_eq!(view.current_tempo_bpm, Some(60.0));
        assert_eq!(view.current_song_pos_beats, Some(1.0));
        assert!(view.is_playing);
    }

    #[test]
    fn synced_capture_completes_each_selected_window_at_aligned_boundaries() {
        for window in WindowLength::ALL {
            let publication = WaveformPublication::new();
            publication.set_selected_window(window);
            let mut engine = CaptureEngine::with_capacity(4.0, 64);
            let target = window_samples(4.0, 60.0, window, 64).expect("test window fits");
            let block = vec![0.5; target];
            engine.process_block(
                &block,
                &block,
                transport(Some(0.0), Some(60.0), true),
                &publication,
            );
            assert_eq!(read(&publication).snapshot_revision, 0);

            engine.process_block(
                &[0.5],
                &[0.5],
                transport(Some(f64::from(window.beats())), Some(60.0), true),
                &publication,
            );
            let view = read(&publication);
            assert_eq!(view.snapshot_revision, 1);
            assert_eq!(view.snapshot_window, window);
            assert_eq!(view.snapshot_beat_index, Some(0));
            assert_eq!(view.sample_count, target);
        }
    }

    #[test]
    fn synced_startup_mid_window_waits_for_the_next_aligned_boundary() {
        let publication = WaveformPublication::new();
        publication.set_selected_window(WindowLength::FourBeats);
        let mut engine = CaptureEngine::with_capacity(4.0, 64);
        engine.process_block(
            &[0.5; 2],
            &[0.5; 2],
            transport(Some(1.0), Some(60.0), true),
            &publication,
        );
        let waiting = read(&publication);
        assert_eq!(waiting.snapshot_revision, 0);
        assert!(!waiting.live_valid);

        engine.process_block(
            &[0.5; 2],
            &[0.5; 2],
            transport(Some(3.5), Some(60.0), true),
            &publication,
        );
        let still_waiting = read(&publication);
        assert_eq!(still_waiting.snapshot_revision, 0);
        assert!(!still_waiting.live_valid);

        engine.process_block(
            &[0.5],
            &[0.5],
            transport(Some(4.0), Some(60.0), true),
            &publication,
        );
        let live = read(&publication);
        assert!(live.live_valid);
        assert_eq!(live.live_window, WindowLength::FourBeats);
        assert_eq!(live.live_beat_index, Some(4));
    }

    #[test]
    fn synced_negative_window_publishes_with_its_negative_start_beat() {
        let publication = WaveformPublication::new();
        publication.set_selected_window(WindowLength::FourBeats);
        let mut engine = CaptureEngine::with_capacity(4.0, 64);
        let block = [0.5; 16];
        engine.process_block(
            &block,
            &block,
            transport(Some(-4.0), Some(60.0), true),
            &publication,
        );
        engine.process_block(
            &[0.5],
            &[0.5],
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        let view = read(&publication);
        assert_eq!(view.snapshot_revision, 1);
        assert_eq!(view.snapshot_beat_index, Some(-4));
        assert_eq!(view.snapshot_window, WindowLength::FourBeats);
    }

    #[test]
    fn seek_discards_partial_capture_and_stop_holds_last_snapshot() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(8.0, 64);
        let left = [0.25; 4];
        let right = [0.75; 4];
        engine.process_block(
            &left,
            &right,
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        engine.process_block(
            &left,
            &right,
            transport(Some(4.0), Some(60.0), true),
            &publication,
        );
        assert_eq!(read(&publication).snapshot_revision, 0);
        engine.process_block(
            &left,
            &right,
            transport(Some(4.5), Some(60.0), true),
            &publication,
        );
        engine.process_block(
            &left,
            &right,
            transport(Some(5.0), Some(60.0), true),
            &publication,
        );
        let revision = read(&publication).snapshot_revision;
        assert_eq!(revision, 1);
        engine.process_block(
            &[],
            &[],
            transport(Some(5.0), Some(60.0), false),
            &publication,
        );
        assert_eq!(read(&publication).snapshot_revision, revision);
    }

    #[test]
    fn sample_scale_seek_discards_a_sub_quarter_partial_capture() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(8.0, 64);
        let left = [0.25; 4];
        let right = [0.75; 4];
        engine.process_block(
            &left,
            &right,
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        engine.process_block(
            &left,
            &right,
            transport(Some(0.7), Some(60.0), true),
            &publication,
        );

        assert_eq!(read(&publication).snapshot_revision, 0);
    }

    #[test]
    fn any_tempo_transition_discards_a_partial_capture() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(8.0, 64);
        let left = [0.25; 4];
        let right = [0.75; 4];
        engine.process_block(
            &left,
            &right,
            transport(Some(0.0), Some(60.0), true),
            &publication,
        );
        engine.process_block(
            &left,
            &right,
            transport(Some(0.5), Some(60.005), true),
            &publication,
        );
        engine.process_block(
            &left,
            &right,
            transport(Some(1.0), Some(60.005), true),
            &publication,
        );

        assert_eq!(read(&publication).snapshot_revision, 0);
    }

    #[test]
    fn missing_position_and_tempo_use_explicit_unsynced_modes() {
        let publication = WaveformPublication::new();
        let mut engine = CaptureEngine::with_capacity(10.0, 32);
        let left = [0.5; 5];
        let right = [0.5; 5];
        engine.process_block(
            &left,
            &right,
            transport(None, Some(120.0), true),
            &publication,
        );
        assert_eq!(read(&publication).snapshot_revision, 1);
        assert_eq!(
            read(&publication).snapshot_mode,
            SnapshotMode::UnsyncedTempo
        );

        engine.process_block(&left, &right, transport(None, None, true), &publication);
        let view = read(&publication);
        assert_eq!(view.snapshot_revision, 2);
        assert_eq!(view.snapshot_mode, SnapshotMode::UnsyncedFallback);
        assert!(view.snapshot_tempo_bpm.is_none());
    }
}
