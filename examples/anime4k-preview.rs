//! Run with --features slint/mcp and SLINT_MCP_PORT to inspect mock UI states.
slint::slint! {
    import { Anime4kSettingsPreview } from "../ui/previews/anime4k-settings-preview.slint";
    export component Preview inherits Anime4kSettingsPreview { }
}

fn main() -> Result<(), slint::PlatformError> {
    use slint::ComponentHandle;
    Preview::new()?.run()
}
