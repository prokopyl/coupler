use clack_extensions::gui::*;

use super::instance::*;
use crate::editor::{Editor, Parent, RawParent};
use crate::plugin::Plugin;

const API: GuiApiType = match GuiApiType::default_for_current_platform() {
    None => panic!("Unsupported platform for GUI"),
    Some(api) => api,
};

impl<'a, P: Plugin> PluginGuiImpl<'a> for MainThreadState<'a, P> {
    fn is_api_supported(&mut self, configuration: GuiConfiguration) -> bool {
        if configuration.is_floating {
            return false;
        }

        configuration.api_type == API
    }

    fn get_preferred_api(&mut self) -> Option<GuiConfiguration> {
        Some(GuiConfiguration {
            is_floating: false,
            api_type: API,
        })
    }

    fn create(&mut self, configuration: GuiConfiguration) -> Result<(), GuiError> {
        if !self.is_api_supported(configuration) {
            return Err(GuiError::CreateError);
        }

        Ok(())
    }

    fn destroy(&mut self) {
        self.editor = None;
    }

    fn set_scale(&mut self, _scale: f64) -> Result<(), GuiError> {
        Err(GuiError::SetScaleError)
    }

    fn get_size(&mut self) -> Option<GuiSize> {
        let size = self.editor.as_ref()?.size();

        Some(GuiSize {
            width: size.width.round() as u32,
            height: size.height.round() as u32,
        })
    }

    fn can_resize(&mut self) -> bool {
        false
    }

    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        None
    }

    fn adjust_size(&mut self, _size: GuiSize) -> Option<GuiSize> {
        None
    }

    fn set_size(&mut self, _size: GuiSize) -> Result<(), GuiError> {
        Err(GuiError::SetSizeError)
    }

    fn set_parent(&mut self, window: Window) -> Result<(), GuiError> {
        #[cfg(target_os = "windows")]
        let raw_parent = window.as_win32_hwnd().map(RawParent::Win32);

        #[cfg(target_os = "macos")]
        let raw_parent = window.as_cocoa_nsview().map(RawParent::Cocoa);

        #[cfg(target_os = "linux")]
        let raw_parent = window.as_x11_handle().map(RawParent::X11);

        let raw_parent = raw_parent.ok_or(GuiError::SetParentError)?;

        let parent = unsafe { Parent::from_raw(raw_parent) };

        self.editor = Some(self.plugin.editor(parent));

        Ok(())
    }

    fn set_transient(&mut self, _window: Window) -> Result<(), GuiError> {
        Err(GuiError::SetTransientError)
    }

    fn suggest_title(&mut self, _title: &str) {}

    fn show(&mut self) -> Result<(), GuiError> {
        Err(GuiError::ShowError)
    }

    fn hide(&mut self) -> Result<(), GuiError> {
        Err(GuiError::HideError)
    }
}
