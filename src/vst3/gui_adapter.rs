//! VST3 adapter for the shared retained Radiant editor.

use std::sync::Arc;

use toybox::vst3::prelude::Steinberg::*;
use toybox::vst3::prelude::*;

use super::shared_state::WaveVst3Shared;

/// VST3 host adapter delegating all editor ownership to Toybox's Radiant facade.
pub(super) struct WaveVst3GuiAdapter {
    gui: toybox::radiant_gui::RadiantHostedGui,
}

impl WaveVst3GuiAdapter {
    /// Construct the editor from the controller's adopted shared state.
    pub(super) fn new(shared: Arc<WaveVst3Shared>) -> Self {
        Self {
            gui: crate::gui::new_gui(Arc::clone(&shared.publication)),
        }
    }
}

impl Vst3HostedGui for WaveVst3GuiAdapter {
    fn set_parent_raw(&mut self, parent: toybox::raw_window_handle::RawWindowHandle) {
        self.gui.set_parent(parent);
    }

    fn open(&mut self) -> bool {
        self.gui.open()
    }

    fn close(&mut self) {
        self.gui.close();
    }

    fn last_size(&self) -> Option<(u32, u32)> {
        self.gui.last_size()
    }

    fn request_resize(&self, width: u32, height: u32) {
        self.gui.request_resize(width, height);
    }

    fn on_key_down(&self, key: char16, key_code: int16, modifiers: int16) -> bool {
        self.gui.on_key_down(key, key_code, modifiers)
    }

    fn on_key_up(&self, key: char16, key_code: int16, modifiers: int16) -> bool {
        self.gui.on_key_up(key, key_code, modifiers)
    }
}
