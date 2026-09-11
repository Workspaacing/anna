use gpui::Pixels;
use settings::{DockSide, IntoGpui as _, RegisterSetting, Settings};

#[derive(Clone, Debug, RegisterSetting)]
pub struct CoworkSettings {
    pub button: bool,
    pub dock: DockSide,
    pub default_width: Pixels,
    pub default_model: String,
    pub catalog_url: String,
    pub max_output_tokens: u64,
}

impl Settings for CoworkSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let cowork = content.cowork.as_ref().unwrap();
        Self {
            button: cowork.button.unwrap(),
            dock: cowork.dock.unwrap(),
            default_width: cowork.default_width.unwrap().into_gpui(),
            default_model: cowork.default_model.clone().unwrap(),
            catalog_url: cowork.catalog_url.clone().unwrap(),
            max_output_tokens: cowork.max_output_tokens.unwrap(),
        }
    }
}
