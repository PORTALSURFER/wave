//! VST3 process-context to format-neutral transport conversion.

use toybox::vst3::prelude::*;

use crate::capture::TransportInfo;

/// Convert one VST3 process context without inventing missing host facts.
pub(super) unsafe fn transport_from_process_context(
    process_context: *mut ProcessContext,
) -> TransportInfo {
    let Some(context) = (unsafe { process_context.as_ref() }) else {
        return TransportInfo::default();
    };
    let state = context.state;
    let tempo_valid = (state & ProcessContext_::StatesAndFlags_::kTempoValid) != 0;
    let position_valid = (state & ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid) != 0;
    TransportInfo {
        tempo_bpm: tempo_valid.then_some(context.tempo),
        song_pos_beats: position_valid.then_some(context.projectTimeMusic),
        is_playing: (state & ProcessContext_::StatesAndFlags_::kPlaying) != 0,
    }
}
