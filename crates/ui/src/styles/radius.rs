use gpui::{Rems, rems};

/// shadcn/ui's "small" radius preset: `--radius: 0.45rem`.
const BASE_RADIUS_IN_REMS: f32 = 0.45;

/// A step on the corner radius scale.
///
/// Every step is a fixed multiple of one base radius, following shadcn/ui's
/// `--radius-sm` to `--radius-xl`, so controls and the surfaces that hold them
/// keep the same proportions. Pixel values are at the default 16px rem size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Radius {
    /// 4.32px (`--radius-sm`). Keycaps, checkboxes and inline code.
    Small,
    /// 5.76px (`--radius-md`). Buttons, chips, tooltips, and menu and list items.
    Medium,
    /// 7.2px (`--radius-lg`). Inputs, large buttons, popovers, menus and banners.
    Large,
    /// 10.08px (`--radius-xl`). Modals and dialogs.
    ExtraLarge,
}

impl Radius {
    /// Returns the radius in rems.
    pub fn rems(self) -> Rems {
        let scale = match self {
            Radius::Small => 0.6,
            Radius::Medium => 0.8,
            Radius::Large => 1.0,
            Radius::ExtraLarge => 1.4,
        };
        rems(BASE_RADIUS_IN_REMS * scale)
    }
}
