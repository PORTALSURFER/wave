//! VST3 edit controller and retained Radiant view creation.

use std::ffi::CStr;
use std::ptr;

use toybox::vst3::prelude::Steinberg::*;
use toybox::vst3::prelude::*;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::gui_adapter::WaveVst3GuiAdapter;
use super::shared_state::WaveVst3Shared;

/// VST3 edit controller for WAVE.
pub(super) struct WaveVst3Controller {
    connection: InstanceConnection<WaveVst3Shared>,
}

impl WaveVst3Controller {
    /// Create an unconnected controller endpoint with default shared state.
    pub(super) fn new() -> Self {
        Self {
            connection: InstanceConnection::new(
                InstanceConnectionRole::Controller,
                WaveVst3Shared::new(),
            ),
        }
    }

    fn shared(&self) -> std::sync::Arc<WaveVst3Shared> {
        self.connection.shared()
    }
}

impl Class for WaveVst3Controller {
    type Interfaces = (IEditController, IConnectionPoint, IToyboxSharedState);
}

toybox::impl_vst3_instance_connection!(WaveVst3Controller, connection);

impl IPluginBaseTrait for WaveVst3Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for WaveVst3Controller {
    unsafe fn setComponentState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> int32 {
        0
    }

    unsafe fn getParameterInfo(&self, _index: int32, _info: *mut ParameterInfo) -> tresult {
        kInvalidArgument
    }

    unsafe fn getParamStringByValue(
        &self,
        _id: ParamID,
        _value_normalized: ParamValue,
        _string: *mut String128,
    ) -> tresult {
        kInvalidArgument
    }

    unsafe fn getParamValueByString(
        &self,
        _id: ParamID,
        _string: *mut TChar,
        _value_normalized: *mut ParamValue,
    ) -> tresult {
        kInvalidArgument
    }

    unsafe fn normalizedParamToPlain(
        &self,
        _id: ParamID,
        value_normalized: ParamValue,
    ) -> ParamValue {
        value_normalized
    }

    unsafe fn plainParamToNormalized(&self, _id: ParamID, plain_value: ParamValue) -> ParamValue {
        plain_value.clamp(0.0, 1.0)
    }

    unsafe fn getParamNormalized(&self, _id: ParamID) -> ParamValue {
        0.0
    }

    unsafe fn setParamNormalized(&self, _id: ParamID, _value: ParamValue) -> tresult {
        kInvalidArgument
    }

    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }

    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = name;
            ptr::null_mut()
        }

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            if name.is_null() {
                return ptr::null_mut();
            }
            let requested = unsafe { CStr::from_ptr(name) };
            let editor = unsafe { CStr::from_ptr(ViewType::kEditor) };
            if requested.to_bytes() != editor.to_bytes() {
                return ptr::null_mut();
            }
            let adapter = WaveVst3GuiAdapter::new(self.shared());
            let view =
                HostedVst3View::new(adapter, crate::gui::WINDOW_WIDTH, crate::gui::WINDOW_HEIGHT)
                    .with_size_bounds(640, 360, 1600, 1000);
            let Some(view) = ComWrapper::new(view).to_com_ptr::<IPlugView>() else {
                return ptr::null_mut();
            };
            ComPtr::into_raw(view)
        }
    }
}
