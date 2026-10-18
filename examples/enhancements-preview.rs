//! Standalone mock UI; no playback, credentials or network services.
slint::slint! {
    import { EnhancementSettingsPreview } from "../ui/previews/enhancement-settings-preview.slint";
    export component Preview inherits EnhancementSettingsPreview { }
}
fn main() -> Result<(), slint::PlatformError> {
    use slint::ComponentHandle;
    Preview::new()?.run()
}
