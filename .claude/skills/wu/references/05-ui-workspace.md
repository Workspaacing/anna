# Wu — UI Shell & Component Layer

Audience: an agent about to write or modify UI code in this repo.
Scope: the design system (`ui`), the component registry (`component`/`component_preview`),
the app shell (`workspace`), and the satellite UI crates (panels, pickers, selectors,
title bar, markdown, inspector). GPUI internals and `editor`/`language`/`project` are
covered by other reports and treated as given.

All paths below are relative to `C:/Users/USER/Documents/wu-main/`.

---

## 0. Orientation — where things live

| Concern | Crate / file |
|---|---|
| Design system (all reusable widgets) | `crates/ui/src/ui.rs` (root), `components/`, `styles/`, `traits/`, `utils/` |
| Prelude every UI file imports | `crates/ui/src/prelude.rs` |
| Component registry / preview trait | `crates/component/src/component.rs` |
| `#[derive(RegisterComponent)]` | `crates/ui_macros/src/derive_register_component.rs` |
| Component gallery (the app view) | `crates/component_preview/src/component_preview.rs` |
| Icon name enum (`IconName`) | `crates/icons/src/icons.rs` |
| File-type icon lookup | `crates/file_icons/src/file_icons.rs` |
| App shell / `Workspace` | `crates/workspace/src/workspace.rs` (17k lines) |
| `Item` / `ItemHandle` / `SerializableItem` | `crates/workspace/src/item.rs` |
| `Panel` / `PanelHandle` / `Dock` | `crates/workspace/src/dock.rs` |
| `Pane` / `PaneGroup` | `crates/workspace/src/pane.rs` (9.7k), `pane_group.rs` |
| `ModalLayer` / `ModalView` | `crates/workspace/src/modal_layer.rs` |
| `StatusBar` / `StatusItemView` / `HideStatusItem` | `crates/workspace/src/status_bar.rs` |
| `Toolbar` / `ToolbarItemView` | `crates/workspace/src/toolbar.rs` |
| Notifications / `Toast` / `show_error` | `crates/workspace/src/notifications.rs` |
| `ToastLayer` / `ToastView` | `crates/workspace/src/toast_layer.rs` + `crates/notifications/src/status_toast.rs` |
| Workspace DB / session restore | `crates/workspace/src/persistence.rs`, `persistence/model.rs`, `crates/db`, `crates/session` |
| Modal list UX | `crates/picker/src/picker.rs` |
| Command palette + filter hooks | `crates/command_palette/src/command_palette.rs`, `crates/command_palette_hooks/src/command_palette_hooks.rs` |
| All action definitions | `crates/wu_actions/src/lib.rs`, plus `crates/menu/src/menu.rs` |
| Global wiring (`init()` call order) | `crates/wu/src/main.rs` (lines ~470-815), `crates/wu/src/wu.rs` |

Repo-wide rules live in `.rules` (both `AGENTS.md` and `CLAUDE.md` contain only the text
`.rules`). Read it: it has a HARD RULE about prepending two `> [!IMPORTANT]` lines to
`README.md` whenever you modify source files, and it forbids `mod.rs`.

---

## 1. Component vocabulary — use these, don't hand-roll

Everything below is exported from `ui` and mostly re-exported through `ui::prelude::*`.
**Import `use ui::prelude::*;` at the top of every UI file.** It brings in `gpui::prelude::*`,
`div/px/rems/relative`, `Styled`, `StyledExt`, `h_flex`/`v_flex`, `Button`, `Icon`, `Label`,
`Color`, `DynamicSpacing`, `theme::ActiveTheme`, and the
`Clickable`/`Disableable`/`Toggleable`/`FixedWidth`/`VisibleOnHover` traits.

### 1.1 Buttons — `crates/ui/src/components/button/`

| Type | Constructor | File |
|---|---|---|
| `Button` (label + optional icons) | `Button::new(id: impl Into<ElementId>, label: impl Into<SharedString>)` | `button/button.rs` |
| `IconButton` | `IconButton::new(id, IconName::X)` | `button/icon_button.rs` |
| `ButtonLike` (escape hatch, arbitrary children) | `ButtonLike::new(id)` | `button/button_like.rs` |
| `ToggleButtonSimple` / `ToggleButtonWithIcon` / `ToggleButtonGroup<T, COLS, ROWS>` | `::new(...)` | `button/toggle_button.rs` |
| `SplitButton` | `SplitButton::new(left, right_any_element)` | `button/split_button.rs` |
| `ButtonLink` (opens a URL) | `ButtonLink::new(label, url)` | `button/button_link.rs` |
| `CopyButton` | `CopyButton::new(id, message)` | `button/copy_button.rs` |

All buttons implement `ButtonCommon: Clickable + Disableable` (`button_like.rs:19`):
`.id()`, `.style(ButtonStyle)`, `.size(ButtonSize)`, `.tooltip(fn)`, `.tab_index(isize)`,
`.layer(ElevationIndex)`, `.track_focus(&FocusHandle)`. Plus `Clickable::on_click` /
`cursor_style`, `Disableable::disabled(bool)`, `Toggleable::toggle_state(bool)`,
`SelectableButton::selected_style(ButtonStyle)`, `FixedWidth::width` / `full_width`.

- `ButtonStyle` = `Filled | Tinted(TintColor) | Outlined | OutlinedGhost | OutlinedCustom(Hsla) | Subtle` (default) `| Transparent`
- `TintColor` = `Accent | Error | Warning | Success`
- `ButtonSize` = `Large(32) | Medium(28) | Default(22) | Compact(18) | None(16)`
- `IconButtonShape` = `Square | Wide`

Buttons already carry ARIA plumbing: `.aria_label`, `.aria_description`, `.aria_value`,
`.aria_role(gpui::Role)`, `.aria_expanded`. Use them rather than rolling your own.

```rust
Button::new("apply-fix", "Apply Fix")
    .style(ButtonStyle::Tinted(TintColor::Accent))
    .start_icon(Icon::new(IconName::Check))
    .key_binding(KeyBinding::for_action(&MyAction, cx))
    .tooltip(Tooltip::text("Applies the suggested fix"))
    .on_click(cx.listener(|this, _event, window, cx| this.apply(window, cx)));
```

### 1.2 Text — `components/label/`, `styles/typography.rs`

- `Label::new(text)` — `#[derive(IntoElement)]`, implements `LabelCommon`.
- `HighlightedLabel::new(text, highlight_indices)` — for fuzzy-match results.
- `LoadingLabel`, `SpinnerLabel`, `LabelLike` (unconstrained escape hatch).
- `Headline` + `HeadlineSize::{XSmall,Small,Medium,Large,XLarge}`.

`LabelCommon` (`components/label/label_like.rs:34`): `.size(LabelSize)`, `.weight(FontWeight)`,
`.line_height_style(LineHeightStyle)`, `.color(Color)`, `.strikethrough()`, `.italic()`,
`.underline()`, `.alpha(f32)`, `.truncate()`, `.single_line()`, `.buffer_font(cx)`, `.inline_code(cx)`.

`StyledTypography` (auto-impl for all `Styled`): `.font_ui(cx)`, `.font_buffer(cx)`,
`.text_ui(cx)` (14px), `.text_ui_sm(cx)` (12px), `.text_ui_lg(cx)` (16px), `.text_ui_xs(cx)`,
`.text_ui_size(TextSize, cx)`. Never hard-code `text_size(px(14.))` — it ignores `ui_scale`.

### 1.3 Icons — `components/icon.rs` + `crates/icons`

- `Icon::new(IconName::Foo)` then `.color(Color)`, `.size(IconSize)`, `.transform(Transformation)`.
- `Icon::from_path(path)` — heuristic: `icons/...` = embedded SVG, otherwise external raster
  from an icon theme.
- `Icon::from_external_svg(svg_string)`.
- `IconWithIndicator::new(icon, Option<Indicator>)`, `DecoratedIcon`, `IconDecoration`, `AnyIcon`.
- `IconSize` = `Indicator(10) | XSmall(12) | Small(14) | Medium(16, default) | XLarge(48) | Custom(Rems)`.

`IconName` is a plain enum in `crates/icons/src/icons.rs` with `strum(serialize_all = "snake_case")`;
`IconName::path()` returns `icons/<snake_case>.svg`. Adding a variant requires an actual
`assets/icons/<name>.svg` — the test at `crates/icons/src/icons.rs:218` (`test_all_icons_exist`)
fails otherwise.

File-type icons: `file_icons::FileIcons::get_icon(path, cx)`,
`get_folder_icon(expanded, path, cx)`, `get_chevron_icon(expanded, cx)`,
`get_icon_for_type(type_str, cx)` — all return `Option<SharedString>` for `Icon::from_path`.

### 1.4 Toggles — `components/toggle.rs`

- `Checkbox::new(id, ToggleState)`, `Switch::new(id, ToggleState)`, `SwitchField::new(...)`
  (label + description + switch row).
- `ToggleState` = `Unselected | Indeterminate | Selected`, with `From<bool>`,
  `From<Option<bool>>`, `.inverse()`, `.selected()`, `ToggleState::from_any_and_all(any, all)`.
- `ToggleStyle`, `SwitchColor`, `SwitchLabelPosition`.

### 1.5 Overlays — tooltips, popovers, menus

**`Tooltip`** (`components/tooltip.rs`) is a `Render` view, not a `RenderOnce` element:

- `Tooltip::text(title)` returns `impl Fn(&mut Window, &mut App) -> AnyView` — drop it straight
  into `.tooltip(...)`.
- `Tooltip::simple(title, cx) -> AnyView`.
- `Tooltip::for_action("Label", &SomeAction, cx)` / `for_action_in(..., &focus_handle, cx)` —
  renders the keybinding next to the label.
- `Tooltip::for_action_title`, `for_action_title_in`, `with_meta`, `with_meta_in`.
- Builder form: `Tooltip::new(title).meta(...).key_binding(...)`, `Tooltip::new_element(fn)`,
  `Tooltip::element(...)`.
- `LinkPreview::new(url, cx)` for hyperlink hovers.

**`ContextMenu`** (`components/context_menu.rs`, ~2200 lines) is a `Render` entity built with
a closure:

```rust
ContextMenu::build(window, cx, |menu, _window, _cx| {
    menu.header("Section")
        .action("Do the thing", Box::new(MyAction))
        .toggleable_entry("Enabled", is_enabled, IconPosition::Start, None, |_w, cx| { /* .. */ })
        .separator()
        .entry("Custom", Some(Box::new(OtherAction)), |window, cx| { /* .. */ })
        .link("Docs", Box::new(OpenDocs))
        .submenu("More", window, cx, |sub, _w, _cx| sub.action("Nested", Box::new(X)))
})
```

Other builders: `build_persistent` (stays open after confirm), `custom_entry`,
`custom_entry_with_docs`, `custom_row`, `entry_with_end_slot`, `entry_with_end_slot_on_hover`,
`action_checked`, `action_checked_with_disabled`, `action_disabled_when`,
`toggleable_entry_disabled_when`, `header_with_link`, `context(focus_handle)`,
`key_context(..)`, `fixed_width(..)`, `keep_open_on_confirm(bool)`, `documentation_aside(..)`.
Individual entries: `ContextMenuEntry::new(label).icon(..).icon_color(..).action(..).handler(..).disabled(..)`.

**`PopoverMenu<M: ManagedView>`** (`components/popover_menu.rs`):
`PopoverMenu::new(id).trigger(IconButton::new(...)).menu(|window, cx| Some(ContextMenu::build(...)))
.anchor(Anchor::TopRight).attach(..).offset(..).with_handle(PopoverMenuHandle::default())`.
Use `.trigger_with_tooltip(trigger, tooltip_fn)` so the trigger tooltip is suppressed while open.
`PopoverMenuHandle<M>` gives `.toggle(window, cx)` for opening a menu from an action —
see the LSP status button at `crates/wu/src/wu.rs:489-497`.

**`right_click_menu(id)`** free function (`components/right_click_menu.rs:75`) returns
`RightClickMenu<M>` with `.trigger(fn)` and `.menu(fn)`; `status_bar.rs` uses it for the
"Hide Button" menu.

**`Popover`** (`components/popover.rs`) — plain styled container, `Popover::new()`.
**`DropdownMenu`** (`components/dropdown_menu.rs`) — `DropdownMenu::new(id, label, menu_entity, ...)`
or `new_with_element(...)`; `DropdownStyle`.

### 1.6 Lists — `components/list/`

- `List::new()` then `.header(ListHeader)`, `.empty_message(..)`, `.toggle(Option<bool>)`, `.children(..)`.
- `ListItem::new(id: impl Into<ElementId>)` — the workhorse row. Builders: `.child(..)`,
  `.start_slot(E)`, `.end_slot(E)`, `.end_hover_slot(E)`, `.toggle_state(bool)`,
  `.selectable(bool)`, `.inset(bool)`, `.spacing(ListItemSpacing::{Dense,Sparse,ExtraDense})`,
  `.indent_level(usize)`, `.indent_step_size(px)`, `.disabled(bool)`, `.on_click(..)`,
  `.on_secondary_mouse_down(..)`, `.on_hover(..)`, `.tooltip(..)`, `.toggle(Option<bool>)` +
  `.on_toggle(..)`, `.outlined()`, `.rounded()`, `.focused(..)`, `.height(..)`,
  `.dock(DockSide)`, plus ARIA (`.aria_role`, `.aria_label`, `.aria_keyshortcuts`,
  `.aria_checked`, `.aria_active_descendant()`).
- `ListHeader::new(label)`, `ListSubHeader::new(label)`, `ListSeparator`, `ListBulletItem`.
- `TreeViewItem::new(id, label)` for hierarchical panels.
- `Navigable::new(child)` + `NavigableEntry::new(&scroll_handle, cx)` for keyboard-navigable
  custom (non-picker) lists.
- `StickyItems`, `IndentGuides` for panel trees.

### 1.7 Modal chrome — `components/modal.rs`

`Modal::new(id, Option<ScrollHandle>)`, `ModalHeader::new()`, `ModalRow::new()`,
`ModalFooter::new()`, `Section::new()` / `Section::new_contained()`, `SectionHeader::new(label)`.
These are the *visual* chrome only. The *behaviour* (dismiss on click-outside/Esc, focus restore)
comes from `workspace::ModalView` + `ModalLayer` — see §4 and recipe (c).

### 1.8 Everything else

| Component | Constructor | Notes |
|---|---|---|
| `KeyBinding` | `KeyBinding::for_action(&action, cx)`, `for_action_in(&action, &focus, cx)`, `from_keystrokes(..)` | `.size(..)`, `.platform_style(..)`, `.disabled(..)`; `Key::new`, `KeyIcon::new` for raw keys |
| `KeybindingHint` | `components/keybinding_hint.rs` | inline "press X" hints |
| `Indicator` | `Indicator::dot()`, `Indicator::bar()`, `Indicator::icon(icon)` | `.color(Color)`, `.border_color(Color)` |
| `Divider` | `Divider::horizontal()`, `vertical()`, `horizontal_dashed()`, `vertical_dashed()` | `.inset()`, `.color(DividerColor)` |
| `Disclosure` | `Disclosure::new(id, is_open)` | `.on_toggle(..)`, `.opened_icon` / `.closed_icon`, `.shape(IconButtonShape)` |
| `Table` | `Table::new(cols)` | `.header(row)`, `.row(row)`, `.striped()`, `.width_config(ColumnWidthConfig::{auto,explicit,redistributable,auto_with_table_width})`, `.uniform_list(..)`, `.variable_row_height_list(..)`, `.interactable(&TableInteractionState)`, `.pin_cols(n)` — `components/data_table.rs` |
| `Banner` | `Banner::new()` | `.severity(Severity)`, `.action_slot(el)`; implements `ParentElement` |
| `Callout` | `Callout::new()` | `.severity`, `.icon`, `.title`, `.description`, `.actions_slot`, `.dismiss_action`, `.border_position` |
| `Avatar` | `Avatar::new(src)` | `.size(..)`, `.grayscale(..)`, `.border_color(..)`, `.indicator(..)`; `AvatarAudioStatusIndicator`, `AvatarAvailabilityIndicator`, `Facepile` |
| `Scrollbars` / `WithScrollbar` | `element.vertical_scrollbar_for(&handle, window, cx)`, `.custom_scrollbars(Scrollbars::new(ScrollAxes::Both)..., window, cx)` | `components/scrollbar.rs`; implemented for `Div` and `Stateful<Div>` |
| `Tab` / `TabBar` | `Tab::new(id)`, `TabBar::new(id)` | used by `Pane`; `TabPosition`, `TabCloseSide` |
| `Chip` | `Chip::new(label)` | small pill |
| `CountBadge`, `DiffStat`, `ProgressBar`, `CircularProgress`, `GradientFade`, `ProjectEmptyState`, `RedistributableColumns` | see `crates/ui/src/components.rs` | |
| `AlertModal`, `AnnouncementToast` | `components/notification/` | |
| `InputField` | `ui_input::InputField::new(window, cx, placeholder)` | single-line text input built on `Editor`; `InputFieldStyle` |

`Severity` = `Info | Success | Warning | Error` (`styles/severity.rs`), shared by `Banner` and `Callout`.

---

## 2. Styling: helpers, traits, spacing, theming

### 2.1 Layout helpers

- `h_flex()` / `v_flex()` free functions (`components/stack.rs`) return a `Div` pre-set to
  `flex().flex_row().items_center()` / `flex().flex_col()`. Use these instead of
  `div().flex().flex_col()`.
- Group variants: `h_group()`, `h_group_sm/lg/xl()`, `v_group()`, ... (`components/group.rs`) —
  flex containers with a standard gap.
- `StyledExt` (`traits/styled_ext.rs`, blanket-impl for every `Styled`): `.h_flex()`, `.v_flex()`,
  `.elevation_1(cx)` / `_2` / `_3` (plus `_borderless` variants), `.border_primary(cx)`,
  `.border_muted(cx)`, and `.debug_bg_red()/green()/blue()/yellow()/cyan()/magenta()` for
  quick layout debugging.

### 2.2 Elevation (`styles/elevation.rs`)

`ElevationIndex` = `Background | Surface | EditorSurface | ElevatedSurface | ModalSurface`.

- `elevation_1` = Surface — title bar, panels, tab bar, editor.
- `elevation_2` = ElevatedSurface — notifications, palettes, floating panels.
- `elevation_3` = ModalSurface — dialogs/modals; per the doc comment, anything at this layer
  MUST dismiss (or prompt) on interaction outside it, otherwise use elevation_2.

`ElevationIndex::shadow(cx)` and `.bg(cx)` give the matching shadow/background.

### 2.3 Spacing (`styles/spacing.rs`, generated by `ui_macros::derive_dynamic_spacing`)

`DynamicSpacing::Base00 ... Base48`. The number is the pixel value at default rem size and
default UI density; each variant returns a density-aware value:
`DynamicSpacing::Base08.rems(cx)` -> `Rems`, `DynamicSpacing::Base08.px(cx)` -> `Pixels`.

The source is explicit: do NOT use `ui_density()` to compute spacing; always use `DynamicSpacing`.
Tailwind-ish shorthands (`.p_2()`, `.gap_1()`) are fine and used pervasively for small fixed gaps,
but anything that should scale with density must go through `DynamicSpacing`.

### 2.4 Units (`styles/units.rs`)

`BASE_REM_SIZE_IN_PX = 16.0`; `rems_from_px(14.0)` instead of `rems(0.875)`;
`vw(percent, window)` / `vh(percent, window)` for viewport-relative lengths.

### 2.5 Colors — never hard-code

Two correct levels:

1. **Semantic `Color` enum** (`styles/color.rs`) for text/icon foreground:
   `Color::{Default, Muted, Hidden, Disabled, Placeholder, Accent, Selected, Hint, Info,
   Success, Warning, Error, Created, Modified, Deleted, Conflict, Ignored, Debugger,
   Player(u32), VersionControlAdded/Modified/Deleted/Conflict/Ignored, Custom(Hsla)}`.
   `Color::color(cx) -> Hsla` resolves against the active theme. The doc comment on `Custom`
   says: "It is highly, HIGHLY recommended not to use this!"

2. **Theme colors** for backgrounds/borders: `cx.theme().colors().<field>` — e.g. `background`,
   `surface_background`, `elevated_surface_background`, `editor_background`, `panel_background`,
   `status_bar_background`, `title_bar_background`, `border`, `border_variant`,
   `element_background`, `ghost_element_background`, `text`, `text_muted`, `text_accent`.
   Status colors: `cx.theme().status().{error,warning,success,info,hint,created,modified,
   deleted,...}` plus their `*_background` / `*_border` variants. Syntax: `cx.theme().syntax()`.
   Players: `cx.theme().styles.player.color_for_participant(i)`.
   Appearance: `cx.theme().appearance` / `.appearance()` -> `Appearance::{Light,Dark}`;
   helper `ui::utils::is_light(cx)`.

`ActiveTheme` is a trait on `App` (`crates/theme/src/theme.rs:146`), pulled in via the prelude.
`hsla(...)` literals in component code are a smell outside `debug_bg_*` and the Windows
close-button red in `platform_windows.rs`.

### 2.6 `FluentBuilder` (`crates/gpui/src/util.rs:11`)

`.map(|this| ..)`, `.when(cond, |this| ..)`, `.when_else(cond, a, b)`,
`.when_some(opt, |this, v| ..)`, `.when_none(&opt, |this| ..)`. Available on elements via
`gpui::prelude::*`; some entity types (e.g. `ContextMenu`) opt in with `impl FluentBuilder for X {}`.

### 2.7 Other traits and utils

`Clickable`, `Disableable`, `Toggleable`/`ToggleState`, `SelectableButton`, `FixedWidth`,
`VisibleOnHover` (`.visible_on_hover(group_name)`), `Transformable` (icon rotation),
`DefaultAnimations`/`CommonAnimationExt` (`traits/animation_ext.rs`, `styles/animation.rs`),
`PlatformStyle::platform()` (`styles/platform.rs`) for Mac/Linux/Windows branching.
Utils (`crates/ui/src/utils/`): `with_rem_size`, `format_distance`, `CornerSolver`,
`inner_corner_radius`, `color_contrast`, `apca_contrast`, `control_characters`, `search_input`,
`reveal_in_file_manager_label`, `buffer_text_style`, `capitalize`, `platform_title_bar_height`,
`TRAFFIC_LIGHT_PADDING`.

---

## 3. `component` + `component_preview` — the registry

### How it works

`crates/component/src/component.rs` defines:

```rust
pub trait Component {
    fn id() -> ComponentId { ComponentId(Self::name()) }
    fn scope() -> ComponentScope { ComponentScope::None }
    fn status() -> ComponentStatus { ComponentStatus::Live }
    fn name() -> &'static str { std::any::type_name::<Self>() }
    fn sort_name() -> &'static str { Self::name() }
    fn description() -> &'static str;                              // required
    fn preview(window: &mut Window, cx: &mut App) -> AnyElement;   // required
}
```

Registration uses the `inventory` crate: `#[derive(RegisterComponent)]` emits an
`inventory::submit!` plus a compile-time assertion that `Component` is implemented
(`crates/ui_macros/src/derive_register_component.rs`). `component::init()` walks the inventory
and fills the global `COMPONENT_DATA` registry — it is already called from
**`workspace::init` (`crates/workspace/src/workspace.rs:843`)**, so you never call it yourself.

- `ComponentScope` = `Agent | Collaboration | DataDisplay | Editor | Images | Input | Layout |
  Loading | Navigation | None | Notification | Overlays | Onboarding | Status | Typography |
  Utilities | VersionControl`
- `ComponentStatus` = `WorkInProgress | EngineeringReady | Live | Deprecated`

### Is a preview expected for new components?

**Yes for anything added to `crates/ui/src/components/`.** Nearly every component there carries
`#[derive(IntoElement, RegisterComponent)]` (usually plus `Documented`) with an `impl Component`
block at the bottom of the same file. It is not enforced by a test, but it is the uniform
convention and the only way the component appears in `workspace: open component preview`.
App-level one-off views (panels, modals, status items) do NOT need previews —
`StatusToast` has one, `OutlinePanel` does not.

### Template to copy

`crates/ui/src/components/disclosure.rs` (smallest complete example) or
`crates/ui/src/components/button/button.rs:515` (richest).

```rust
use crate::component_prelude::*;  // Component, ComponentScope, single_example,
                                  // example_group[_with_title], Documented, RegisterComponent
use crate::prelude::*;

/// This doc comment becomes the description when you derive `Documented`.
#[derive(IntoElement, Documented, RegisterComponent)]
pub struct MyThing { id: ElementId, label: SharedString }

impl MyThing {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self { /* .. */ }
}

impl RenderOnce for MyThing {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement { /* .. */ }
}

impl Component for MyThing {
    fn scope() -> ComponentScope { ComponentScope::DataDisplay }
    fn sort_name() -> &'static str { "MyThingA" }   // groups related components when sorted
    fn description() -> &'static str { Self::DOCS } // from `Documented`
    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex().gap_6().children(vec![
            example_group_with_title("Variants", vec![
                single_example("Default", MyThing::new("a", "Hi").into_any_element()),
            ]),
        ]).into_any_element()
    }
}
```

Then add `mod my_thing;` and `pub use my_thing::*;` to `crates/ui/src/components.rs`.

### The gallery

`crates/component_preview/src/component_preview.rs` is a `SerializableItem` workspace tab opened
via the `workspace::OpenComponentPreview` action (registered in `component_preview::init`, called
from `crates/wu/src/main.rs:812`). It reads `component::components()` and renders each
`ComponentMetadata::preview()`. Its DB table lives in `crates/component_preview/src/persistence.rs` —
a 59-line complete `db::Domain` + `db::static_connection!` + `query!` example, and the best
template for per-item persistence.

---

## 4. `workspace` — the shell

### Structure

```
Window
+- MultiWorkspace (multi_workspace.rs)      window-level container, system window tabs
   +- Workspace (workspace.rs)
      +- titlebar_item: Option<AnyView>     set via workspace.set_titlebar_item (:2942)
      +- left_dock / bottom_dock / right_dock : Entity<Dock>  -> Vec<PanelEntry>
      +- center: PaneGroup                  -> Member::{Axis(PaneAxis), Pane(Entity<Pane>)}
      |  +- Pane -> TabBar + Toolbar + Vec<Box<dyn ItemHandle>>
      +- status_bar: Entity<StatusBar>      -> left_items / right_items
      +- modal_layer: Entity<ModalLayer>    -> at most one ActiveModal
      +- toast_layer: Entity<ToastLayer>    -> at most one ActiveToast
      +- notifications: Vec<(NotificationId, AnyView)>
```

### The `Item` trait (`crates/workspace/src/item.rs:173`)

What a new editor-tab type must implement:

```rust
pub trait Item: Focusable + EventEmitter<Self::Event> + Render + Sized {
    type Event;
    fn tab_content_text(&self, detail: usize, cx: &App) -> SharedString;   // ONLY required method
    // everything below has a default:
    fn tab_content(&self, params: TabContentParams, window: &Window, cx: &App) -> AnyElement;
    fn tab_icon(&self, ..) -> Option<Icon>;
    fn tab_tooltip_text / tab_tooltip_content(..);
    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent));
    fn for_each_project_item(&self, cx: &App, f: &mut dyn FnMut(EntityId, &dyn project::ProjectItem));
    fn buffer_kind(&self, cx) -> ItemBufferKind;    // None | Singleton | Multibuffer
    fn active_project_path(&self, cx) -> Option<ProjectPath>;
    fn can_split(&self) -> bool;
    fn clone_on_split(&self, workspace_id, window, cx) -> Task<Option<Entity<Self>>>;
    fn is_dirty(&self, cx) -> bool;
    fn has_conflict / has_deleted_file(..);
    fn can_save / can_save_as / save / save_as / reload(..);
    fn confirm_close(&self, cx) -> Option<CloseConfirmation>;
    fn deactivated / discarded / on_removed / workspace_deactivated / pane_changed(..);
    fn navigate(..) -> bool;  fn set_nav_history(..);  fn include_in_nav_history() -> bool;
    fn breadcrumb_location(..) -> ToolbarItemLocation;  fn breadcrumbs(..);  fn breadcrumb_prefix(..);
    fn as_searchable(..) -> Option<Box<dyn SearchableItemHandle>>;
    fn act_as_type(..);  fn added_to_workspace(..);  fn show_toolbar(&self) -> bool;
    fn preserve_preview(..);  fn handle_drop(..);  fn capability(..);  fn toggle_read_only(..);
    fn suggested_filename(..);  fn pixel_position_of_cursor(..);
}
```

Note the defaults that panic when misused: `clone_on_split` panics unless `can_split()` is true;
`save` / `save_as` / `reload` panic unless `can_save()` is true.

`ItemHandle` (`item.rs:482`) is the object-safe side, blanket-implemented for `Entity<T: Item>`;
downcast with `item.downcast::<Editor>()`.

`SerializableItem: Item` (`item.rs:415`) adds `serialized_item_kind()`, `deserialize(project,
workspace, workspace_id, item_id, window, cx)`, `serialize(&mut self, workspace, item_id, closing,
cx) -> Option<Task<Result<()>>>`, `should_serialize(&self, event) -> bool`, and
`cleanup(workspace_id, alive_items, window, cx)`.

`ProjectItem: Item` (`item.rs:1146`) is for items opened *from a file path* (Editor, ImageView).

Registration (both in `workspace.rs`):
- `workspace::register_project_item::<I>(cx)` — line 998; lets "open this path" produce your item.
- `workspace::register_serializable_item::<I>(cx)` — line 1076; enables session restore.

### The `Panel` trait (`crates/workspace/src/dock.rs:38`)

Required: `persistent_name()`, `panel_key()`, `position(window, cx) -> DockPosition`,
`position_is_valid(DockPosition)`, `set_position(..)`, `default_size(window, cx) -> Pixels`,
`icon(window, cx) -> Option<IconName>`, `icon_tooltip(..) -> Option<&'static str>`,
`toggle_action() -> Box<dyn Action>`, `activation_priority() -> u32`.
Supertraits: `Focusable + EventEmitter<PanelEvent> + Render + Sized`.

Optional/defaulted: `activation_focus_handle`, `min_size`, `initial_size_state`,
`size_state_changed`, `supports_flexible_size` / `has_flexible_size` / `set_flexible_size`,
`icon_label`, `is_zoomed` / `set_zoomed`, `starts_open`, `set_active`, `pane()`, `remote_id()`,
`enabled(cx)`, `is_agent_panel()`, `hide_button_setting(cx) -> Option<HideStatusItem>`.

`DockPosition` = `Left | Bottom | Right`. `PanelHandle` (`dock.rs:108`) is the object-safe side
and adds `move_to_next_position`.

Documented gotcha (`dock.rs:41-48`): `Focusable::focus_handle` must be the *root* of the panel's
focus subtree and must be tracked by the panel's root element; `activation_focus_handle` (the
handle focused on activation — e.g. a filter editor) must be a focus-tree descendant of it.
Otherwise Zen-mode auto-close and toggle-focus misbehave.

Existing panels: `ProjectPanel`, `OutlinePanel`, `TerminalPanel`, `GitPanel`, `DebugPanel`
(`grep -rn "impl Panel for"`).

### Modals

`workspace::ModalView: ManagedView` (`modal_layer.rs:48`) with optional
`on_before_dismiss(..) -> DismissDecision::{Dismiss(bool), Pending}`, `fade_out_background()`,
`render_bare()`. `ManagedView` = `Render + EventEmitter<DismissEvent> + Focusable`.

- Show: `workspace.toggle_modal(window, cx, |window, cx| MyModal::new(..))` (`workspace.rs:6899`)
- Query: `workspace.active_modal::<V>(cx)`
- Hide: `workspace.hide_modal(window, cx)`

Only one modal at a time. Dismissed pickers are *stashed* (not dropped) so `ReopenLastPicker`
can restore them with exact prior state; `Picker::reopenable(false, cx)` opts out (the command
palette does).

### Status bar

`StatusItemView: Render` (`status_bar.rs:38`) requires
`set_active_pane_item(Option<&dyn ItemHandle>, window, cx)` and
`hide_setting(&self, cx) -> Option<HideStatusItem>`. Add with
`workspace.status_bar().update(cx, |bar, cx| bar.add_left_item(entity, window, cx))`,
`add_right_item`, or `insert_item_after(position, ..)`; `position_of_item::<T>()` finds an index.

### Toolbar

`ToolbarItemView: Render + EventEmitter<ToolbarItemEvent>` (`toolbar.rs:24`):
`set_active_pane_item(..) -> ToolbarItemLocation`, plus optional `pane_focus_update` and
`contribute_context`. `ToolbarItemLocation` = `Hidden | PrimaryLeft | PrimaryRight | Secondary`.
`crates/breadcrumbs/src/breadcrumbs.rs` is the canonical minimal implementation.

### Notifications / toasts

Two distinct systems:

1. **Notification stack** (top-right, persistent) — `crates/workspace/src/notifications.rs`:
   `workspace.show_notification(NotificationId, cx, build)`, `workspace.show_error(err, cx)`,
   `workspace.show_toast(Toast::new(id, msg).autohide(), cx)`, `dismiss_notification`,
   `suppress_notification`, `unsuppress`, `clear_all_notifications`.
   `NotificationId::{unique::<T>(), composite::<T>(id), named(s)}`.
   `Notification: EventEmitter<DismissEvent> + EventEmitter<SuppressEvent> + Focusable + Render`.
   Default body type: `simple_message_notification::MessageNotification`
   (`.primary_message`, `.primary_icon`, `.primary_on_click`, `.secondary_*`, `.more_info_url`,
   `.title`, `.button_style`, `.show_close_button`, `.show_suppress_button`, `.auto_hide`).
   App-global (windowless) notifications: `show_app_notification(id, cx, build)`, replayed into
   new workspaces by `show_initial_notifications`.

2. **Status toast** (bottom-right, transient, single slot) — `crates/workspace/src/toast_layer.rs`:
   `ToastView: ManagedView` with `action() -> Option<ToastAction>` and `auto_dismiss() -> bool`;
   shown with `workspace.toggle_status_toast(entity, cx)`. Default duration 10s.
   Standard implementation: `notifications::status_toast::StatusToast`.

Result/Task helpers (`notifications.rs:1531+`):
`NotifyResultExt::{notify_err(workspace, cx), notify_workspace_async_err(weak, cx), notify_app_err(cx)}`
and `NotifyTaskExt::detach_and_notify_err(workspace_weak, window, cx)`.

Blocking OS-style prompt: `window.prompt(PromptLevel::Critical, title, Some(detail), &["A","B"], cx)`
returns a future of the chosen index; rendered by `crates/ui_prompt` unless the
`use_system_prompts` workspace setting is on (macOS/Windows only).

### Serialization / session restore

- `crates/workspace/src/persistence.rs` (6.2k lines) — `WorkspaceDb` (sqlez) with the `query!`
  macro; `SerializedPane`, `SerializedItem` and friends live in `persistence/model.rs`.
- Item bodies persist themselves through `SerializableItem`; the kind -> descriptor mapping is
  `SerializableItemRegistry` (`workspace.rs:1017-1096`), a `Global`.
- Panel state persists two ways: dock layout via `WorkspaceDb`, and per-panel blobs via
  `db::kvp::KeyValueStore` keyed by `format!("{PANEL_KEY}-{workspace_or_session_id}")` —
  see `OutlinePanel::serialization_key` / `serialize` (`crates/outline_panel/src/outline_panel.rs:906-938`).
- `crates/session/src/session.rs` — `Session::id()`, `AppSession::last_session_id()`,
  `last_session_window_stack()`, `persist_id(cx)`. Used for restore-on-launch and for keying
  KVP entries before a workspace DB id exists.
- New per-item tables: declare a `db::Domain` with `MIGRATIONS`, then
  `db::static_connection!(MyDb, [WorkspaceDb]);` — template: `crates/component_preview/src/persistence.rs`.

### Handy `Workspace` methods

`toggle_modal`, `hide_modal`, `active_modal::<V>`, `toggle_status_toast`,
`add_item_to_active_pane(item, destination_index, focus, window, cx)`, `add_item_to_center`,
`split_item(SplitDirection, item, window, cx)`, `open_paths`, `open_abs_path`, `open_path`,
`active_pane()`, `adjacent_pane_of(&pane, window, cx)`, `panel::<T>(cx)`, `focus_panel::<T>`,
`toggle_panel_focus::<T>`, `close_panel::<T>`, `add_panel(entity, window, cx)`,
`set_titlebar_item`, `status_bar()`, `project()`, `database_id()`, `session_id()`,
`register_action(..)`, `with_local_workspace(..)`, and the free function
`workspace::with_active_or_new_workspace(cx, |workspace, window, cx| ..)`.

---

## 5. STEP-BY-STEP RECIPES

### (a) Add a new dock panel

**Copy-templates**
- Trait shape / minimal complete impl: `crates/workspace/src/dock.rs:1640-1791` (`TestPanel`).
- Real panel: `crates/outline_panel/src/outline_panel.rs` — `init` at :653, `load` at :668,
  `new` at :700, `serialization_key`/`serialize` at :906-938, `impl Panel` at :4957.

1. **New crate** `crates/my_panel/` with `Cargo.toml` containing
   `[lib] name = "my_panel"` and `path = "src/my_panel.rs"` (no `lib.rs`, no `mod.rs` — `.rules`).
   Add it to the workspace `Cargo.toml` members and to `crates/wu/Cargo.toml` dependencies.
2. **Define the toggle action**: `actions!(my_panel, [ToggleFocus, Toggle]);` in your crate
   (or add to `crates/wu_actions/src/lib.rs` for a `wu::`-namespaced one).
3. **Struct + `Render` + `Focusable` + `EventEmitter<PanelEvent>` + `impl Panel`:**

```rust
impl Panel for MyPanel {
    fn persistent_name() -> &'static str { "MyPanel" }
    fn panel_key() -> &'static str { MY_PANEL_KEY }
    fn position(&self, _: &Window, cx: &App) -> DockPosition {
        MyPanelSettings::get_global(cx).dock.into()
    }
    fn position_is_valid(&self, p: DockPosition) -> bool {
        matches!(p, DockPosition::Left | DockPosition::Right)
    }
    fn set_position(&mut self, p: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        settings::update_settings_file(self.fs.clone(), cx, move |s, _| { /* write dock */ });
    }
    fn default_size(&self, _: &Window, cx: &App) -> Pixels {
        MyPanelSettings::get_global(cx).default_width
    }
    fn icon(&self, _: &Window, cx: &App) -> Option<IconName> { Some(IconName::ListTree) }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> { Some("My Panel") }
    fn toggle_action(&self) -> Box<dyn Action> { Box::new(ToggleFocus) }
    fn activation_priority(&self) -> u32 { 7 }   // must be distinct from other panels
    fn hide_button_setting(&self, _: &App) -> Option<workspace::HideStatusItem> {
        Some(workspace::HideStatusItem::new(|s| { /* set button = Some(false) */ }))
    }
}
```

4. **`pub async fn load(workspace: WeakEntity<Workspace>, cx: AsyncWindowContext) ->
   Result<Entity<Self>>`** — read the KVP blob, then
   `workspace.update_in(&mut cx, |ws, window, cx| Self::new(ws, serialized.as_ref(), window, cx))`.
   Copy `outline_panel.rs:668-700` verbatim; it is exactly the shape `add_panel_when_ready` wants.
5. **`pub fn init(cx: &mut App)`:**

```rust
cx.observe_new(|workspace: &mut Workspace, _, _| {
    workspace.register_action(|ws, _: &ToggleFocus, window, cx| {
        ws.toggle_panel_focus::<MyPanel>(window, cx);
    });
}).detach();
```

6. **Wire it up:** add `my_panel::init(cx);` in `crates/wu/src/main.rs` (near
   `outline_panel::init(cx);`, ~line 635), and inside `initialize_panels` in
   `crates/wu/src/wu.rs:640-678` add `let my_panel = MyPanel::load(workspace_handle.clone(),
   cx.clone());` plus a `add_panel_when_ready(my_panel, workspace_handle.clone(), cx.clone())`
   entry in the `futures::join!`.
7. Optional persistence: `SerializedMyPanel` serde struct + `serialization_key(workspace)` +
   `serialize(cx)` using `db::kvp::KeyValueStore::global(cx)` — copy `outline_panel.rs:906-938`.

### (b) Add a new workspace item / tab type

**Copy-templates, smallest first**
- `crates/workspace/src/theme_preview.rs` (425 lines) — non-file-backed tab; `impl Item` is
  25 lines at :86 (`tab_content_text`, `can_split`, `clone_on_split`).
- `crates/svg_preview/src/svg_preview_view.rs` (336 lines) — a complete, non-serializable item:
  `register()` at :248, `open_preview_in_pane` / `open_preview_to_the_side_of_pane` at :205-223,
  `activate_or_add_preview` at :225, `Render` at :279, `impl Item` at :315.
  `crates/svg_preview/src/svg_preview.rs` (24 lines) is the entire `init()`.
- Serializable tab: `crates/component_preview/src/component_preview.rs:709` (`impl Item`) and
  `:777` (`impl SerializableItem`) plus `crates/component_preview/src/persistence.rs`.
  Other examples: `crates/keymap_editor`, `crates/markdown_preview`, `crates/terminal_view`,
  `crates/onboarding`, `crates/image_viewer`, `crates/git_ui/src/project_diff.rs`.

Steps:
1. Struct holding a `FocusHandle`; `impl Focusable`, `impl EventEmitter<YourEvent>`, `impl Render`.
   The root element must `.track_focus(&self.focus_handle(cx))` and set `.key_context("MyView")`.
2. `impl Item for MyView { type Event = ...; fn tab_content_text(..) -> SharedString { .. } }`.
   Add `tab_icon`, `to_item_events`, `can_split` + `clone_on_split` as needed.
3. An action (in `wu_actions` or a local `actions!(..)`) plus
   `pub fn register(workspace: &mut Workspace, window, cx)` calling
   `workspace.register_action(|ws, _: &MyAction, window, cx| { .. })`.
4. Open it with `workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx)`,
   or `pane.update(cx, |pane, cx| pane.add_item(Box::new(view), true, true, None, window, cx))`,
   or `workspace.split_item(SplitDirection::Right, Box::new(view), window, cx)`.
   For "open beside": `workspace.adjacent_pane_of(&pane, window, cx)`.
   To reuse an existing tab, scan `pane.read(cx).items_of_type::<MyView>()` first
   (see `svg_preview_view.rs:234`).
5. `pub fn init(cx: &mut App) { cx.observe_new(|ws: &mut Workspace, window, cx| { ..
   MyView::register(..) }).detach(); }` and call it from `crates/wu/src/main.rs`.
6. **To survive restart**: also `impl SerializableItem` and call
   `workspace::register_serializable_item::<MyView>(cx)` in `init`.
   **To open when a file path is opened**: `impl ProjectItem` and call
   `workspace::register_project_item::<MyView>(cx)`.

### (c) Add a new modal (picker-based)

**Copy-template: `crates/line_ending_selector/src/line_ending_selector.rs` — 195 lines, the whole
crate, and it contains every moving part.** Next smallest:
`crates/encoding_selector/src/encoding_selector.rs` (320).
Search + side preview: `crates/project_symbols/src/project_symbols.rs` (626) — shows
`Picker::uniform_list_with_preview` with `picker_preview::editor_preview`.
Full-featured references: `crates/file_finder/src/file_finder.rs`,
`crates/command_palette/src/command_palette.rs`, `crates/tab_switcher`, `crates/theme_selector`.
Non-picker modal (custom widget): `crates/go_to_line/src/go_to_line.rs`.

`PickerDelegate` (`crates/picker/src/picker.rs:164`) — required methods:

```rust
type ListItem: IntoElement;                 // usually `ui::ListItem`
fn name() -> &'static str;                  // serialization key; renaming breaks persistence
fn match_count(&self) -> usize;
fn selected_index(&self) -> usize;
fn set_selected_index(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Picker<Self>>);
fn placeholder_text(&self, window: &mut Window, cx: &mut App) -> Arc<str>;
fn update_matches(&mut self, query: String, window, cx) -> Task<()>;
fn confirm(&mut self, secondary: bool, window, cx);
fn dismissed(&mut self, window, cx);
fn render_match(&self, ix: usize, selected: bool, window, cx) -> Option<Self::ListItem>;
```

Useful optional hooks: `no_matches_text`, `separators_after_indices`, `render_header`,
`render_footer`, `searchbar_trailer`, `render_editor`, `actions_menu`, `confirm_input`,
`confirm_completion`, `confirm_update_query`, `select_child` / `select_parent`,
`finalize_update_matches` (blocking sync match so the palette does not flash empty),
`editor_position`, `should_dismiss`, `select_history`, `can_select`, `select_on_hover`,
`selected_index_changed`, `has_another_open_menu`, multi-select
(`supports_multi_select`, `is_item_selected`, `toggle_item_selected`, `selected_item_count`,
`confirm_multi`, `clear_selection`, `render_match_with_checkbox`), preview
(`try_get_preview_data_for_match`, `preview_layout_changed`).

Picker constructors: `Picker::uniform_list(delegate, window, cx)` (fixed row height),
`Picker::list(..)` (variable height), `Picker::nonsearchable_uniform_list(..)` (no query editor),
`Picker::uniform_list_with_preview(..)`, `list_with_preview(..)`,
`list_with_preview_and_query_editor(..)`. Chainable: `.max_height(..)`,
`.show_scrollbar(bool)`, `.reopenable(bool, cx)`, `.list_measure_all()`.

Two ways to show it:

1. **No wrapper (simplest)** — `Picker<D>` already implements `ModalView` (`picker.rs:1998`):

```rust
workspace.toggle_modal(window, cx, move |window, cx| {
    Picker::uniform_list(MyDelegate::new(handle, project.clone()), window, cx)
});
```
(see `crates/project_symbols/src/project_symbols.rs:27`)

2. **With a wrapper struct** — needed when you want custom chrome, or a stable entity the
   delegate can emit `DismissEvent` on. The `LineEndingSelector` pattern: a struct holding
   `picker: Entity<Picker<Delegate>>`; `impl Render` = `v_flex().child(self.picker.clone())`;
   `impl Focusable` delegating to `self.picker.focus_handle(cx)`;
   `impl EventEmitter<DismissEvent>`; `impl ModalView`. The delegate holds a
   `WeakEntity<Wrapper>` and does
   `self.wrapper.update(cx, |_, cx| cx.emit(DismissEvent)).ok();` in `dismissed()`.

Register the trigger in `init`:

```rust
// workspace-scoped
pub fn init(cx: &mut App) { cx.observe_new(MyModal::register).detach(); }
fn register(workspace: &mut Workspace, _window: Option<&mut Window>, _: &mut Context<Workspace>) {
    workspace.register_action(|ws, _: &Toggle, window, cx| { /* ws.toggle_modal(..) */ });
}

// app-global (works even with no workspace yet) — theme_selector.rs:31, settings_profile_selector.rs:11
cx.on_action(|_: &Toggle, cx| {
    workspace::with_active_or_new_workspace(cx, |ws, window, cx| { /* .. */ });
});

// editor-scoped — line_ending_selector.rs:22
cx.observe_new(LineEndingSelector::register).detach();
fn register(editor: &mut Editor, _window: Option<&mut Window>, cx: &mut Context<Editor>) {
    let handle = cx.weak_entity();
    editor.register_action(move |_: &Toggle, window, cx| Self::toggle(&handle, window, cx)).detach();
}
```

### (d) Add a status bar item

**Copy-template: `crates/line_ending_selector/src/line_ending_indicator.rs` — 80 lines, complete.**
Also: `crates/workspace/src/active_file_name.rs` (77), `crates/image_viewer/src/image_info.rs` (110),
`crates/go_to_line/src/cursor_position.rs` (328),
`crates/activity_indicator/src/activity_indicator.rs:867` (an item that hides itself).

1. `#[derive(Default)] pub struct MyIndicator { .., _observe_active_editor: Option<Subscription> }`
2. `impl Render` — return a bare `div()` early when
   `StatusBarSettings::get_global(cx).<x>_button` is false. Use
   `Button::new(id, text).label_size(LabelSize::Small).tab_index(0isize).tooltip(..)`.
3. ```rust
   impl StatusItemView for MyIndicator {
       fn set_active_pane_item(&mut self, active: Option<&dyn ItemHandle>, window, cx) {
           if let Some(editor) = active.and_then(|i| i.downcast::<Editor>()) {
               self._observe_active_editor = Some(cx.observe_in(&editor, window, Self::update));
               self.update(editor, window, cx);
           } else {
               self._observe_active_editor = None;
           }
           cx.notify();
       }
       fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
           Some(HideStatusItem::new(|settings| {
               settings.status_bar.get_or_insert_default().my_button = Some(false);
           }))
       }
   }
   ```
4. Wire in `crates/wu/src/wu.rs`: construct it around lines 470-502 and add
   `status_bar.add_right_item(my_indicator, window, cx);` inside the
   `workspace.status_bar().update(..)` block at `crates/wu/src/wu.rs:503-516`.
   Current left items: lsp_button, diagnostic_summary, active_file_name, git_blame_status,
   activity_indicator. Right items: encoding, language, toolchain, line ending, cursor position,
   image info. Note `render_right_tools` iterates `.rev()`, so the LAST `add_right_item` renders
   leftmost.

### (e) Register a command in the command palette

There is **no palette registry**. The palette calls `window.available_actions(cx)` and shows
everything not filtered out (`crates/command_palette/src/command_palette.rs:113-125`). So:

1. **Define the action.** Either
   `actions!(my_namespace, [/** doc comment shown to the user */ MyAction]);` in your crate, or
   in `crates/wu_actions/src/lib.rs` for `wu::`-namespaced ones. For actions carrying data use
   `#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)] #[action(namespace = wu)]`.
   Deprecated names go in `#[action(deprecated_aliases = ["old::Name"])]`.
2. **Make it dispatchable in the current focus context** so it shows up in `available_actions`:
   `workspace.register_action(|ws, _: &MyAction, window, cx| { .. })` inside
   `cx.observe_new(|workspace: &mut Workspace, _, _| { .. })`, or `editor.register_action(..)`,
   or `.on_action(cx.listener(..))` on an element, or `cx.on_action(|_: &MyAction, cx| ..)` for
   an app-global action.
3. The palette label is `humanize_action_name("my_namespace::MyAction")` ->
   `"my namespace: my action"` (`command_palette.rs:692`). The action's **doc comment is shown
   to the user** — write it as UI copy, not as a code note.
4. Add a default keybinding in `assets/keymaps/default-{windows,macos,linux}.json` if desired.

**Hiding / filtering** — `command_palette_hooks::CommandPaletteFilter` (a `Global`):

```rust
CommandPaletteFilter::update_global(cx, |filter, _cx| {
    filter.hide_namespace("editor");                             // whole namespace
    filter.hide_action_types(&[TypeId::of::<MyDebugAction>()]);  // specific actions
    filter.show_action_types(&[TypeId::of::<MyAction>()]);       // overrides a hidden namespace
});
```

`is_hidden` checks `shown_action_types` first, then `hidden_namespaces` / `hidden_action_types`.
Real usages: `crates/inspector_ui/src/inspector_ui.rs:22` (hide `dev::ToggleInspector` in
release builds), `crates/extensions_ui/src/extensions_ui.rs:98`,
`crates/language_tools/src/syntax_tree_view.rs:46`, `crates/command_palette/src/command_palette.rs:900`.

**Intercepting the query** (e.g. Vim `:` commands):
`GlobalCommandPaletteInterceptor::set(cx, |query, workspace, cx| -> Task<CommandInterceptResult>)`,
returning `CommandInterceptItem { action, string, positions }` plus `exclusive: bool`
(true suppresses the normal matches). `::clear(cx)` removes it.

### (f) Show a notification / toast / error

```rust
// 1. Error from a Result inside a Workspace context
some_result.notify_err(workspace, cx);                   // NotifyResultExt
some_result.notify_app_err(cx);                          // no workspace handle available
task.detach_and_notify_err(workspace_weak, window, cx);  // NotifyTaskExt

// 2. Explicit error notification (deduped by NotificationId::unique::<E>())
workspace.show_error(anyhow!("Could not do X: {e}"), cx);

// 3. Auto-hiding toast in the notification stack (5s)
workspace.show_toast(
    Toast::new(NotificationId::unique::<MyThing>(), "Saved 3 files")
        .on_click("Undo", |window, cx| { /* .. */ })
        .autohide(),
    cx,
);

// 4. Rich notification with primary / secondary buttons
workspace.show_notification(NotificationId::unique::<MyThing>(), cx, |cx| {
    cx.new(|cx| MessageNotification::new("Extension installed", cx)
        .primary_message("Reload")
        .primary_icon(IconName::RotateCw)
        .primary_on_click(|window, cx| { /* .. */ })
        .secondary_message("Later"))
});

// 5. Bottom-right status toast (single slot, auto-dismiss, one action)
let toast = StatusToast::new("Pushed to origin/main", cx, |this, _cx| {
    this.icon(Icon::new(IconName::GitBranch).size(IconSize::Small).color(Color::Muted))
        .action("Create Pull Request", |window, cx| {
            window.dispatch_action(Box::new(wu_actions::git::CreatePullRequest), cx)
        })
});
workspace.toggle_status_toast(toast, cx);

// 6. Blocking OS-style prompt
let answer = window.prompt(PromptLevel::Critical, "Unsupported GPU", Some(&detail),
                           &["Skip", "Troubleshoot and Quit"], cx);
```

Templates: `crates/git_ui/src/git_panel.rs:4941` (StatusToast with action),
`crates/git_ui_core/src/notifications.rs:53`, `crates/wu/src/wu.rs:625` (`window.prompt`),
`crates/workspace/src/notifications.rs:1227-1330` (the `WorkspaceError` impls, which is how
`show_error` chooses icon/title/severity for a given error type).

---

## 6. `title_bar` vs `platform_title_bar` (Windows notes)

- **`crates/platform_title_bar`** is the *chrome*: drag region, window controls, client-side
  decoration rounding, system window tabs. `PlatformTitleBar::new(id, cx)` is a `Render` entity
  that also implements `ParentElement` (children = the app-specific content).
  `PlatformTitleBar::init(cx)` registers `SystemWindowTabs`. Height comes from
  `ui::utils::platform_title_bar_height(window)`.
- **`crates/title_bar`** is the *content*: project/branch menus (`WorktreePicker`), the
  application menu (`application_menu.rs`), the onboarding banner, the update-available button,
  collaboration bits. `title_bar::init(cx)` calls `PlatformTitleBar::init` and then
  `cx.observe_new(|workspace, ..| workspace.set_titlebar_item(..))`.
  `Workspace` stores it as `titlebar_item: Option<AnyView>` (`workspace.rs:1339`, rendered at :7801).

**Windows-specific concerns** (`crates/platform_title_bar/src/platforms/platform_windows.rs`):

- `render_right_window_controls` returns `WindowsWindowControls::new(height)` for
  `PlatformStyle::Windows`; Mac returns `None` (traffic lights are OS-drawn, padded with
  `TRAFFIC_LIGHT_PADDING`); Linux draws `LinuxWindowControls` from `WindowButtonLayout`
  and only when `Decorations::Client`.
- Caption glyphs are **font codepoints**, not SVG icons: `\u{e921}` minimize, `\u{e923}` restore,
  `\u{e922}` maximize, `\u{e8bb}` close. Font is `Segoe Fluent Icons` on build >= 22000
  (detected at runtime with `RtlGetVersion`), else `Segoe MDL2 Assets`.
- The close button hover colour is the hard-coded Windows red `rgb(232,17,32)` — the one
  legitimate hard-coded colour in this layer.
- Each control sets `.window_control_area(WindowControlArea::{Min,Max,Close})`; the bar itself
  sets `WindowControlArea::Drag`.
- **On Windows the title bar drag / double-click behaviour is handled by the platform layer**
  (explicit comment at `platform_title_bar.rs:206`). The `on_click` double-click handlers are
  gated to Mac, and the `zoom_window` handler to Linux. Do not add Windows equivalents.
- Enablement follows `window.is_minimizable()` / `window.is_resizable()`.
- Client-side decoration rounding (`Decorations::Client { tiling, .. }`) is a Linux path; on
  Windows you get `Decorations::Server`. The same `match window.window_decorations()` block is
  duplicated in `crates/workspace/src/status_bar.rs` — keep them in sync if you touch rounding.
  `status_bar.rs` also carries a Wayland-only 1px gap fix.
- `platform_title_bar/src/system_window_tabs.rs` (529 lines) implements the tab strip below the
  title bar: `MergeAllWindows`, `MoveTabToNewWindow`, `ShowNextWindowTab`,
  `ShowPreviousWindowTab`, `DraggedWindowTab`.

---

## 7. `markdown` + `markdown_preview`

- **`crates/markdown`** — `Markdown` is an *entity*, created with
  `cx.new(|cx| Markdown::new(source: SharedString, Option<Arc<LanguageRegistry>>,
  Option<LanguageName>, cx))`, or `Markdown::new_with_options(.., MarkdownOptions, cx)`, or
  `Markdown::new_text(source, cx)` (links-only parsing — use for plain text that may contain URLs).
- Render with `MarkdownElement::new(markdown_entity, style)` where `style: MarkdownStyle`.
- **Do not build `MarkdownStyle` by hand.** Use
  `MarkdownStyle::themed(MarkdownFont::{Editor,Preview}, window, cx)` or
  `themed_with_overrides(font, colors, syntax, window, cx)` (`crates/markdown/src/markdown.rs:163`).
  Ready-made per-context helpers you can reuse:
  `editor::hover_popover::hover_markdown_style(window, cx)`,
  `editor::hover_popover::diagnostics_markdown_style(window, cx)`,
  `workspace::notifications::markdown_style(window, cx)` (`notifications.rs:431`),
  and `ui_prompt`'s internal `markdown_style(main_message, window, cx)`.
- Other public knobs: `MarkdownOptions`, `CopyButtonVisibility`, `WrapButtonVisibility`,
  `CodeBlockRenderer`, `AutoscrollBehavior`, `ParsedMarkdown`, `RenderedMarkdown`,
  `HeadingLevelStyles`, `BlockQuoteKindColors`.
- Features already handled: text selection (`selection.rs`), search highlights, code-block
  copy/wrap buttons, syntax highlighting through `cx.theme().syntax()`, GitHub-style block-quote
  kinds (note/tip/important/warning/caution), `path_range.rs` for `file.rs#L10-20` links,
  image loading by source offset.
- **`crates/markdown_preview`** — `MarkdownPreviewView` is a `SerializableItem` tab; `init` at
  `markdown_preview.rs:38` registers it plus `markdown::{OpenPreview, OpenPreviewToTheSide,
  OpenFollowingPreview, ScrollPageUp/Down, ScrollToTop/Bottom, CloseAndReturnToEditor}`.
  It is the model for "a preview tab that follows the active editor".

Consumers to imitate: LSP hovers (`crates/editor/src/hover_popover.rs`), notifications
(`crates/workspace/src/notifications.rs` — `LanguageServerPrompt`), prompts (`crates/ui_prompt`),
remote-connection errors (`crates/remote_connection`).
Runnable demos: `crates/markdown/examples/markdown.rs` and `markdown_as_child.rs`.

---

## 8. `inspector_ui` — the built-in UI inspector / devtools

- Entry point: `crates/inspector_ui/src/inspector_ui.rs`. Gated on
  `#[cfg(any(debug_assertions, feature = "inspector"))]`; in release builds `init` only registers
  a handler that reports "dev::ToggleInspector is only available in debug builds and Nightly"
  and hides the action from the command palette.
- Action: `wu_actions::dev::ToggleInspector`. Default keybindings:
  **`shift-alt-i` on Windows** (`assets/keymaps/default-windows.json:593`),
  `ctrl-alt-i` on Linux, `cmd-alt-i` on macOS.
- `crates/inspector_ui/src/inspector.rs:11` does three things:
  `cx.on_action(|_: &ToggleInspector, cx| .. window.toggle_inspector(cx))`,
  `cx.register_inspector_element(|id, state: &DivInspectorState, window, cx| ..)`, and
  `cx.set_inspector_renderer(Box::new(render_inspector))`.
- `crates/inspector_ui/src/div_inspector.rs` (735 lines) is the payload. For the picked element
  it shows: the **Rust source location** (file:line of the `div()` that created it), the
  **computed layout state** (`render_layout_state`, :576), and an **editable JSON style buffer**
  backed by a real `Editor` with JSON LSP support (a hidden `project::Project` is created in
  `inspector.rs` just for this). Style edits apply live to the inspected element.

**Using it to debug layout:** run a debug build, press `shift-alt-i`, hover/click the element,
read the source location to jump to the exact `div()` in code, then tweak the JSON style live
before committing the change to source. For quick box visualisation without the inspector, use
`StyledExt::debug_bg_red()` / `debug_bg_green()` / etc. and remove them before committing.

---

## 9. Conventions

### File / module naming

- **No `mod.rs`** (`.rules`): use `src/components/button.rs` plus `src/components/button/*.rs`.
- Crate lib roots are named after the crate and declared explicitly in `Cargo.toml`:
  `[lib] name = "ui"`, `path = "src/ui.rs"`. Same for `workspace/src/workspace.rs`,
  `picker/src/picker.rs`, `component/src/component.rs`. New crates must follow this.
- One component family per file/dir; `crates/ui/src/components.rs` is a flat
  `mod x; ... pub use x::*;` list — add both lines when adding a component.
- Every crate exposes `pub fn init(cx: &mut App)`, called from `crates/wu/src/main.rs`.
  Crates that contain only actions/types also expose an empty `pub fn init()` purely to defeat
  link-time elision (`crates/wu_actions/src/lib.rs:13`, `crates/menu/src/menu.rs:10` explain why).
- Settings structs live in `<crate>/src/<crate>_settings.rs` with `#[derive(RegisterSetting)]`
  and `impl Settings { fn from_settings(content: &SettingsContent) -> Self }`.
- Status-bar / toolbar companions live next to their modal:
  `line_ending_selector/src/line_ending_indicator.rs`, `language_selector/src/active_buffer_language.rs`,
  `encoding_selector/src/active_buffer_encoding.rs`, `toolchain_selector/src/active_toolchain.rs`,
  `go_to_line/src/cursor_position.rs`.

### `RenderOnce` vs `Render` in practice

| Use | When | How you get one |
|---|---|---|
| `RenderOnce` + `#[derive(IntoElement)]` | Stateless; rebuilt every frame; `render(self, window, cx: &mut App)` consumes `self`. Everything in `crates/ui/src/components/` except `ContextMenu`, `Tooltip`, `Scrollbars`. | `MyThing::new(..)` used directly as a `.child(..)` |
| `Render` (an entity / view) | Owns state across frames; needs `Context<Self>`, subscriptions, `cx.notify()`, `cx.listener`, event emission. Panels, modals, status items, `ContextMenu`, `Tooltip`, `Picker<D>`, `Toolbar`, `StatusBar`. | `cx.new(\|cx\| MyView::new(..))` -> `Entity<MyView>` |

`RenderOnce::render` receives `&mut App`, not `Context<Self>` — it cannot call `cx.listener`, so
the parent must pass closures in. `Entity<V: Render>` implements `IntoElement`, so you embed a
view in an element tree with `.child(self.picker.clone())`.

### `#[derive(IntoElement)]`

Required on any struct implementing `RenderOnce` that you want to pass to `.child(..)`.
Common combinations in this repo:
- `#[derive(IntoElement, RegisterComponent)]` — a design-system component with a preview.
- `#[derive(IntoElement, Documented, RegisterComponent)]` — same, doc comment used as description.
- `#[derive(Clone, IntoElement, ...)]` — when the component must be stored/cloned (`Icon`, `KeyBinding`).
- Plain `#[derive(IntoElement)]` — app-local one-off elements (e.g. `WindowsWindowControls`).
`IntoElement` also works on enums (`AnyIcon`, `WindowsCaptionButton`).

### How `IntoElement` components take builder args

- Constructor takes identity plus the single mandatory value:
  `Button::new(id, label)`, `Icon::new(name)`, `ListItem::new(id)`,
  `Checkbox::new(id, ToggleState)`, `Disclosure::new(id, is_open)`, `Table::new(cols)`.
- Everything else is `fn foo(mut self, ..) -> Self`.
- Argument types by convention: `impl Into<SharedString>` for text, `impl Into<ElementId>` for
  ids, `impl Into<Option<X>>` for optional values (so callers can pass `Some(x)`, `x`, or `None`),
  `impl IntoElement` / `E: IntoElement` for slots, `impl Into<DefiniteLength>` for sizes.
- Handlers: `impl Fn(&ClickEvent, &mut Window, &mut App) + 'static`.
  Tooltips: `impl Fn(&mut Window, &mut App) -> AnyView + 'static`.
- Implement `ParentElement` (backed by a `SmallVec<[AnyElement; 2]>` field) when the component
  accepts children — `Banner`, `Modal`, `Section`, `ModalHeader`, `ListItem`, `PlatformTitleBar`.
- Implement `Styled` by forwarding to an inner `Div`/`StyleRefinement` if callers should be able
  to restyle the component (`Divider` does this).

### `ElementId` conventions for lists

`ElementId` has `From` impls for `&'static str`, `SharedString`, `usize`,
`(&'static str, usize)`, `ElementId::NamedInteger`, and more. Observed usage, best first:

- **`ListItem::new(ix)`** for a single flat picker/uniform list — the index is unique within the
  list. Used by `command_palette:616`, `file_finder:2085`, `encoding_selector:313`,
  `component_preview:372`, `extension_version_selector:223`, `git_graph:197`, `call_hierarchy:759`.
- **`ListItem::new(("prefix", ix))`** when several lists coexist in one view:
  `ListItem::new(("changed-file", ix))`, `ListItem::new(("dev-extension-list-item", mat.candidate_id))`.
- **`ListItem::new(SharedString::from(format!("scope-{var_ref}")))`** when identity must be stable
  across reordering (tree views, variable lists, breakpoints) — `debugger_ui` does this.
- Static ids for singletons: `div().id("status-bar")`, `h_flex().id("breadcrumb-container")`,
  `IconButton::new("change-line-ending", ..)`, `v_flex().id("SvgPreview")`.
- Avoid `format!` ids in hot lists when an index or `(str, usize)` tuple works — it allocates
  every frame.

### Other conventions worth copying

- Root element of any focusable view:
  `.track_focus(&self.focus_handle(cx))` plus `.key_context("MyView")` (or a built `KeyContext`
  from a `dispatch_context()` helper, as in `outline_panel.rs:941`).
- Subscriptions stored in fields named `_subscription` / `_subscriptions: Vec<Subscription>`
  (leading underscore = kept alive, never read).
- Tasks stored in fields so they cancel on drop: `_refresh: Task<()>`,
  `pending_serialization: Task<()>`.
- Settings read: `MySettings::get_global(cx)`; settings write:
  `settings::update_settings_file(fs, cx, |s, _| ..)`.
- `cx.observe_new(|workspace: &mut Workspace, window, cx| ..).detach()` is *the* hook for wiring
  per-workspace actions from a crate's `init`.
- `.detach()` / `.detach_and_log_err(cx)` on tasks you intentionally fire and forget; never
  `let _ = fallible()` (`.rules`).

---

## 10. Footguns

1. **`.rules` HARD RULE.** Any source modification requires prepending
   `> [!IMPORTANT]` and `> Remove this line to confirm you've reviewed this PR before submitting.`
   as the first two lines of `README.md`. Never remove them yourself, even if asked to clean up.
2. **Never create `mod.rs`.** New crates need an explicit `[lib] path = "src/<crate>.rs"`.
3. **Hard-coded colours and sizes are wrong.** Use `Color::*` or
   `cx.theme().colors()` / `.status()`; use `DynamicSpacing::BaseNN.rems(cx)` and `text_ui*(cx)`
   so UI density and `ui_scale` work. `Color::Custom(Hsla)` is explicitly discouraged in its own
   doc comment.
4. **`ui_density()` is not for spacing.** `crates/ui/src/styles/spacing.rs` says so explicitly —
   always route spacing through `DynamicSpacing`.
5. **Adding an `IconName` variant without the SVG breaks the build** —
   `crates/icons/src/icons.rs:218` asserts `assets/icons/<snake_case>.svg` exists for every variant.
6. **`PickerDelegate::name()` is a persistence key.** Renaming it silently drops saved picker state.
   The doc comment says as much.
7. **Panel focus-handle contract** (`dock.rs:41-48`): `Focusable::focus_handle` must be the
   subtree root *and* be tracked by the root element; `activation_focus_handle` must be a
   descendant. Getting it wrong breaks Zen-mode auto-close and panel toggle-focus.
8. **`activation_priority()` must be distinct** across panels — it drives dock ordering/activation
   (outline panel uses 6).
9. **`add_right_item` order is reversed on screen** — `render_right_tools` iterates `.rev()`.
10. **`StatusItemView::hide_setting` returning `None`** is only acceptable when the item already
    hides itself based on another user-visible setting (see the doc comment at `status_bar.rs:9-17`);
    otherwise the user has no way to hide your button.
11. **Only one modal and one status toast at a time.** `toggle_modal` replaces the active modal;
    if the current modal's `on_before_dismiss` returns `Dismiss(false)` or `Pending`, your new
    modal silently never appears.
12. **`Picker<D>` is already a `ModalView`** (`picker.rs:1998`). Wrapping it in a struct just to
    call `toggle_modal` is unnecessary — only wrap when you need custom chrome or a stable entity
    the delegate can emit `DismissEvent` on.
13. **Defining an action is not enough for the command palette.** It must be dispatchable from the
    current focus context so it appears in `window.available_actions(cx)`.
14. **Action doc comments are user-facing strings** in the palette and keymap editor.
15. **Palette filtering is global and sticky.** `hide_namespace("editor")` hides everything in the
    namespace; only `show_action_types` re-exposes individual actions.
16. **Do not add drag / double-click handlers for the Windows title bar** — the platform layer
    owns it (`platform_title_bar.rs:206`). Mac and Linux need the explicit handlers; Windows does not.
17. **Two different "toast" concepts.** `Toast` + `workspace.show_toast` = notification stack
    (top-right, stacked). `StatusToast` + `workspace.toggle_status_toast` = `ToastLayer`
    (bottom-right, single slot, 10s). They are not interchangeable.
18. **`register_serializable_item` alone persists nothing.** You also need the `SerializableItem`
    impl, a `should_serialize` that returns true for the right events, and usually your own DB
    table (`db::Domain` + `db::static_connection!`).
19. **`component::init()` is already called by `workspace::init` (`workspace.rs:843`).** Conversely,
    a component in a crate that never gets linked into the binary will not register — this is the
    `inventory` + link-elision problem that the empty `wu_actions::init()` / `menu::init()` exist to solve.
20. **`Tooltip` is a view, `Label` is an element.** `.tooltip()` wants
    `impl Fn(&mut Window, &mut App) -> AnyView`; pass `Tooltip::text("..")` (which *returns* that
    closure) or a closure calling `Tooltip::for_action(..)`. `Tooltip::new(..)` alone is the
    builder for the view body, not the argument.
21. **`Item` defaults panic.** `clone_on_split` panics unless `can_split()` returns true;
    `save` / `save_as` / `reload` panic unless `can_save()` returns true. Override in pairs.
22. **`derive_inspector_reflection` on `StyledExt` is gated off for rust-analyzer** (it costs ~10s
    to expand). Keep the `#[cfg_attr(all(.., not(rust_analyzer)), ..)]` gate intact if you add methods.
23. **Never silently discard errors** (`.rules`): use `?`, `.log_err()`, `.notify_err(..)`,
    `.detach_and_log_err(cx)`, or an explicit `match` — never `let _ = fallible()`.
24. **Build with `./script/clippy`**, not `cargo clippy` (`.rules`). Do not run cargo builds
    speculatively in a large repo like this one.
25. **`ContextMenu::build` vs `build_persistent`.** The plain `build` closes on every confirm;
    use `build_persistent` (which stores the builder so it can re-run) for menus with toggles the
    user will flip several times.
26. **The `ui` prelude does not include `FluentBuilder` for entities.** Elements get `.when()` via
    `gpui::prelude::*`, but a custom struct needs an explicit `impl FluentBuilder for X {}`.
27. **`workspace::with_active_or_new_workspace(cx, ..)`** is required for app-global actions
    (`cx.on_action`) that need a workspace — there may be no window open when the action fires.

---

## 11. Appendix — crate-by-crate map of this domain

| Crate | Entry / key file | Kind | Notes |
|---|---|---|---|
| `ui` | `src/ui.rs` | design system | components + styles + traits + utils; `prelude` and `component_prelude` |
| `ui_input` | `src/input_field.rs` | component | `InputField::new(window, cx, placeholder)`, a `Render` view over `Editor` |
| `ui_macros` | `src/ui_macros.rs` | proc-macro | `derive_dynamic_spacing!`, `#[derive(RegisterComponent)]` |
| `ui_prompt` | `src/ui_prompt.rs` | prompt renderer | `ZedPromptRenderer`; `init` swaps between system and in-app prompts based on `use_system_prompts` |
| `component` | `src/component.rs` | registry | `Component` trait, `ComponentRegistry`, `single_example`, `example_group[_with_title]` |
| `component_preview` | `src/component_preview.rs` | workspace item | the gallery; `workspace::OpenComponentPreview`; `persistence.rs` is the minimal DB template |
| `icons` | `src/icons.rs` | data | `IconName` enum, `path()`, existence test |
| `file_icons` | `src/file_icons.rs` | data | path -> icon path, folder/chevron icons, icon-theme aware |
| `workspace` | `src/workspace.rs` | shell | see §4; also `activity_bar.rs`, `multi_workspace.rs`, `security_modal.rs`, `welcome.rs`, `theme_preview.rs`, `searchable.rs`, `history_manager.rs`, `path_link.rs`, `workspace_error.rs`, `invalid_item_view.rs` |
| `panel` | `src/panel.rs` (35 lines) | traits | `PanelHeader`, `PanelTabs`, `panel::{NextPanelTab, PreviousPanelTab}` — mostly a stub |
| `picker` | `src/picker.rs` | modal list | `PickerDelegate`, `Picker`, plus `head.rs`, `footer.rs`, `preview.rs`, `parts.rs`, `render/`, `popover_menu.rs` (`PickerPopoverMenu`), `persistence.rs`, `highlighted_match_with_paths.rs` |
| `picker_preview` | `src/picker_preview.rs` | helper | `picker_preview::editor_preview(project, window, cx)` for side previews |
| `title_bar` | `src/title_bar.rs` | view | project/branch menu, app menu, onboarding banner, update button |
| `platform_title_bar` | `src/platform_title_bar.rs` | chrome | window controls per platform; `platforms/platform_windows.rs`, `platforms/platform_linux.rs`, `system_window_tabs.rs` |
| `menu` | `src/menu.rs` (36 lines) | actions | `menu::{Cancel, Confirm, SecondaryConfirm, SelectPrevious/Next/First/Last, SelectChild/Parent, Restart, EndSlot}` — the vocabulary every list/menu/picker binds |
| `notifications` | `src/notifications.rs`, `src/status_toast.rs` | components | `StatusToast` (a `ToastView`) |
| `project_panel` | `src/project_panel.rs` (7.9k) | dock panel | file tree; heaviest panel example |
| `outline_panel` | `src/outline_panel.rs` (8.2k) | dock panel | best *complete* panel reference (load/serialize/settings/scrollbars/indent guides) |
| `terminal_view` | `src/terminal_panel.rs` (3.3k), `src/terminal_view.rs` (3.3k) | dock panel + item | a panel that hosts a `Pane` of items |
| `tab_switcher` | `src/tab_switcher.rs` (898) | picker modal | ctrl-tab switcher |
| `file_finder` | `src/file_finder.rs` (2.2k) | picker modal | also `OpenPathPrompt`; the multi-select reference |
| `command_palette` | `src/command_palette.rs` (1.3k) | picker modal | `humanize_action_name`, `persistence.rs` for MRU |
| `command_palette_hooks` | `src/command_palette_hooks.rs` (153) | globals | `CommandPaletteFilter`, `GlobalCommandPaletteInterceptor` |
| `recent_projects` | `src/recent_projects.rs` (3.1k) | picker modal | plus `remote_servers.rs`, `remote_connections.rs`, `wsl_picker.rs`, `disconnected_overlay.rs`, `sidebar_recent_projects.rs` |
| `go_to_line` | `src/go_to_line.rs` (1064) | non-picker modal | plus `cursor_position.rs` status item |
| `project_symbols` | `src/project_symbols.rs` (626) | picker modal | picker + editor preview, no wrapper struct |
| `theme_selector` | `src/theme_selector.rs` (716) | picker modal | plus `icon_theme_selector.rs`; app-global `cx.on_action` registration |
| `language_selector` | `src/language_selector.rs` (676) | picker modal | plus `active_buffer_language.rs` status item |
| `toolchain_selector` | `src/toolchain_selector.rs` (1164) | picker modal | plus `active_toolchain.rs` status item |
| `encoding_selector` | `src/encoding_selector.rs` (320) | picker modal | plus `active_buffer_encoding.rs` status item |
| `line_ending_selector` | `src/line_ending_selector.rs` (195) | picker modal | **smallest complete picker + status item pair — the best template** |
| `settings_profile_selector` | `src/settings_profile_selector.rs` (731) | picker modal | app-global action |
| `breadcrumbs` | `src/breadcrumbs.rs` | toolbar item | canonical `ToolbarItemView` |
| `activity_indicator` | `src/activity_indicator.rs` | status item | LSP/extension/git progress; `hide_setting` returns `None` by design |
| `onboarding` | `src/onboarding.rs` | workspace item | `Onboarding` + `WelcomePage`, both serializable |
| `language_onboarding` | — | banners | per-language onboarding prompts |
| `image_viewer` | `src/image_viewer.rs` (1403) | project + serializable item | plus `image_info.rs` status item |
| `svg_preview` | `src/svg_preview_view.rs` (336) | workspace item | **smallest complete non-serializable item template** |
| `markdown` | `src/markdown.rs` (6.7k) | rendering | `Markdown` entity + `MarkdownElement`; `MarkdownStyle::themed` |
| `markdown_preview` | `src/markdown_preview_view.rs` (3.7k) | serializable item | follows the active editor |
| `tabular_data_preview` | `src/tabular_data_preview.rs` (370) | workspace item | CSV/TSV table view; uses `ui::Table` |
| `inspector_ui` | `src/inspector.rs`, `src/div_inspector.rs` | devtools | debug-only; `shift-alt-i` on Windows |
| `keymap_editor` | `src/keymap_editor.rs` (2.8k+) | serializable item | table-driven editor over the keymap |
| `settings_ui` | `src/settings_ui.rs` | item/window | `OpenSettings`, `OpenSettingsAt`; renderer registry |
| `extensions_ui` | `src/extensions_ui.rs` | workspace item | extension gallery; `extension_version_selector.rs` picker |
| `snippets_ui` | `src/snippets_ui.rs` (67) | actions only | `ConfigureSnippets`, `open_folder` — tiny `observe_new` + `register_action` template |
| `diagnostics` | `src/diagnostics.rs` | items + status | `ProjectDiagnosticsEditor`, `BufferDiagnosticsEditor`, `items::DiagnosticIndicator` (status item), `diagnostic_renderer.rs` |
