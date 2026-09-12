use gpui::Pixels;
use settings::{DockSide, IntoGpui as _, RegisterSetting, Settings};
use task::Shell;

#[derive(Clone, Debug, RegisterSetting)]
pub struct CoworkSettings {
    pub button: bool,
    pub dock: DockSide,
    pub default_width: Pixels,
    pub catalog_url: String,
    pub disabled_models: Vec<String>,
    pub verification: VerificationSettings,
    /// `None` means "whatever the terminal is set to", which is the default.
    pub shell: Option<Shell>,
    pub auto_approve: bool,
}

/// Which of the built-in checks run after the agent changes something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerificationSettings {
    pub format: bool,
    pub diagnostics: bool,
    pub secret_scan: bool,
    pub dependency_audit: bool,
}

impl VerificationSettings {
    /// Whether any check at all is enabled, so the turn loop can skip the work entirely.
    pub fn any_enabled(&self) -> bool {
        self.format || self.diagnostics || self.secret_scan || self.dependency_audit
    }
}

impl Settings for CoworkSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let cowork = content.cowork.as_ref().unwrap();
        Self {
            button: cowork.button.unwrap(),
            dock: cowork.dock.unwrap(),
            default_width: cowork.default_width.unwrap().into_gpui(),
            catalog_url: cowork.catalog_url.clone().unwrap(),
            disabled_models: cowork.disabled_models.clone().unwrap(),
            verification: {
                let verification = cowork.verification.as_ref().unwrap();
                VerificationSettings {
                    format: verification.format.unwrap(),
                    diagnostics: verification.diagnostics.unwrap(),
                    secret_scan: verification.secret_scan.unwrap(),
                    dependency_audit: verification.dependency_audit.unwrap(),
                }
            },
            shell: cowork.shell.clone().map(settings_shell_to_task_shell),
            auto_approve: cowork.auto_approve.unwrap(),
        }
    }
}

/// The settings enum and the runtime enum describe the same three choices but are separate types,
/// so that settings can be deserialized without depending on the task crate.
fn settings_shell_to_task_shell(shell: settings::Shell) -> Shell {
    match shell {
        settings::Shell::System => Shell::System,
        settings::Shell::Program(program) => Shell::Program(program),
        settings::Shell::WithArguments {
            program,
            args,
            title_override,
        } => Shell::WithArguments {
            program,
            args,
            title_override,
        },
    }
}
