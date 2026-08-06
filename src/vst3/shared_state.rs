//! Shared VST3 state and audio-owned runtime construction.

use std::sync::Arc;

use crate::capture::{CaptureEngine, WaveformPublication};

/// State shared by the VST3 processor, controller, and hosted Radiant view.
pub(super) struct WaveVst3Shared {
    /// Lock-free waveform publication consumed by the retained editor.
    pub(super) publication: Arc<WaveformPublication>,
}

impl WaveVst3Shared {
    /// Create one endpoint's initial shared state.
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            publication: Arc::new(WaveformPublication::new()),
        })
    }
}

/// Complete runtime owned by the serialized VST3 process callback.
pub(super) struct WaveVst3Runtime {
    /// Realtime-safe beat capture state.
    pub(super) capture: CaptureEngine,
}

impl WaveVst3Runtime {
    /// Build all sample-rate-dependent storage on a non-audio thread.
    pub(super) fn new(sample_rate: f64) -> Self {
        Self {
            capture: CaptureEngine::new(sample_rate),
        }
    }
}
