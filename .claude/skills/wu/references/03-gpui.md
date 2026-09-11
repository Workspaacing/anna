# GPUI in `wu` — deep notes for coding agents

Scope: `crates/gpui/` + `gpui_apple`, `gpui_linux`, `gpui_macos`, `gpui_windows`, `gpui_platform`,
`gpui_web`, `gpui_wgpu`, `gpui_macros`, `gpui_tokio`, `gpui_util`, `gpui_shared_string`.

This file deliberately **does not** repeat repo-root `.rules` (Context types, `Entity<T>`,
`cx.spawn`/`background_spawn`, `Task`, `Render`/`RenderOnce`, elements, actions, `notify`,
`subscribe`). Everything here is what `.rules` leaves out.

> **Read this first — fork deltas.** This is a Zed fork and GPUI has been restructured. If you
> write GPUI code from memory of upstream Zed you will produce code that does not compile.

---

## 0. Fork deltas vs. upstream Zed GPUI

| Upstream habit | What this repo actually has |
|---|---|
| `gpui::Application::new()` | `gpui_platform::application()` / `::headless()`. `Application::with_platform(Rc<dyn Platform>)` is the only ctor in `gpui` itself. |
| Platform impls inside `crates/gpui/src/platform/{mac,windows,linux}` | Separate crates: `gpui_windows`, `gpui_macos` (+`gpui_apple`), `gpui_linux`, `gpui_web`. `crates/gpui/src/platform/` now holds only *traits* + the test platform. |
| `Task` defined in `gpui::executor` | `Task`, `Priority`, `FallibleTask`, `DedicatedExecutor` are **re-exported from the `scheduler` crate** (`crates/gpui/src/executor.rs:9-11`). |
| `impl_actions!` / `impl_internal_actions!` | **Deleted.** Zero occurrences in the tree. Use `actions!` or `#[derive(Action)]` + `#[action(namespace = ...)]`. |
| `zed::NoAction` | `wu::NoAction`, plus a new `wu::Unbind("some::Action")` (`crates/gpui/src/action.rs:425-458`). |
| `cx.spawn(\|this, mut cx\| async move { ... })` | `cx.spawn(async move \|this, cx\| { ... })` — the arg is an `AsyncFnOnce` (`crates/gpui/src/app/context.rs:237-245`). |
| Blade renderer on Linux | Blade is gone. `gpui_wgpu` (wgpu 29) on Linux + web; DirectX 11 on Windows; Metal on macOS. |
| `#[derive(IntoElement)]` → `RenderOnce` wrapper | Now generates `gpui::ViewElement<Self>` via the new **`View`** trait (`crates/gpui/src/view.rs:170-225`). `Entity<T: Render>` implements `IntoElement` directly, so `.child(my_entity)` works without `.into_any_element()`. |
| — | New: `window.use_state` / `use_keyed_state` React-style hooks (`window.rs:3745-3785`). |
| — | New: full AccessKit a11y layer (`Element::a11y_role`, `.aria_*()`, `crates/gpui/src/window/a11y.rs`). |
| — | New: `#[gpui::property_test]` (proptest), `#[gpui::bench]` (criterion), `profiler` feature with frame journal + hang detection. |
| — | New: `container_query()` element, `Priority` on spawns, `spawn_dedicated`, `VisualTestAppContext`. |

---

## 1. `crates/gpui/` layout

`Cargo.toml` highlights (`crates/gpui/Cargo.toml`):

* `[lib] path = "src/gpui.rs"`, `doctest = false`. Crate root is **`src/gpui.rs`**, not `lib.rs`
  (repo-wide convention — see `.rules`; never create `mod.rs`).
* Published crate (`publish = true`, v0.2.2, homepage gpui.rs). Keep public API doc'd —
  `#![warn(missing_docs)]` is on (`src/gpui.rs:2`).
* `extern crate self as gpui;` (line 7) — that's why in-crate code writes `use crate as gpui;`
  before invoking `actions!` (see `src/action.rs:426`, `src/keymap.rs:297`).
* `#[doc(hidden)] pub mod private { anyhow, inventory, schemars, serde, serde_json }` — macro
  plumbing. Never reference from hand-written code.
* `mod seal { pub trait Sealed {} }` — `Entity<T>` and `AssetLogger` are sealed.

### Module ownership map

| Module | Owns |
|---|---|
| `action.rs` | `Action` trait, `actions!` macro, `ActionRegistry`, `NoAction`/`Unbind`, `ActionBuildError` |
| `app.rs` + `app/` | `App`, `Application`, `Context<T>`, `AsyncApp`, `EntityMap`/leak detection, all test/bench/headless contexts |
| `arena.rs` | Bump arena for per-frame element allocation (`ElementArenaScope`) |
| `asset_cache.rs`, `assets.rs` | `Asset` trait, `Resource` (Uri/Path/Embedded), `AssetSource`, `RenderImage`, `Image` |
| `bounds_tree.rs` | Hitbox/quad spatial index used by the scene |
| `color.rs`, `colors.rs` | `Hsla`, `Rgba`, named colors, `Background`/gradients |
| `element.rs` | `Element`, `IntoElement`, `Render`, `RenderOnce`, `ParentElement`, `AnyElement`, `GlobalElementId` |
| `elements/` | `div`, `img`, `svg`, `text`, `list`, `uniform_list`, `canvas`, `anchored`, `deferred`, `animation`, `surface`, `container_query`, `image_cache` |
| `executor.rs` | `BackgroundExecutor`, `ForegroundExecutor`, `TaskExt::detach_and_log_err` |
| `platform_scheduler.rs` | Bridges `PlatformDispatcher` -> `scheduler::Scheduler` |
| `geometry.rs` | `Pixels`/`DevicePixels`/`Rems`, `Point`/`Size`/`Bounds`/`Edges`/`Corners`, `px()`, `rems()`, `relative()` |
| `gestures.rs`, `spring.rs` | Trackpad gestures, spring animation curves |
| `global.rs` | `Global` marker trait, `ReadGlobal`, `UpdateGlobal` |
| `input.rs`, `interactive.rs` | `PlatformInput`, mouse/key event structs, `EntityInputHandler` |
| `inspector.rs` | `InspectorElementId`, live element inspector (feature `inspector` or `debug_assertions`) |
| `key_dispatch.rs` | `DispatchTree`, focus paths, action dispatch — read its module doc (lines 1-51) |
| `keymap.rs`, `keymap/` | `Keymap`, `KeyBinding`, `KeyContext`, `KeyBindingContextPredicate` |
| `path_builder.rs`, `scene.rs`, `svg_renderer.rs` | Lyon path building, `Scene`/primitive batching, resvg rasterization |
| `platform.rs`, `platform/` | **All platform traits**, `Keystroke`, `TestPlatform`/`TestWindow`/`TestDispatcher`, `ThreadedDispatcher` |
| `profiler.rs`, `profiler/` | Frame journal, hang detection, task timing (feature `profiler`) |
| `queue.rs` | Priority MPMC queue used by Windows/Linux/wasm dispatchers |
| `style.rs`, `styled.rs` | `Style`, `StyleRefinement`, the Tailwind-ish `Styled` trait |
| `subscription.rs` | `SubscriberSet`, `Subscription` |
| `tab_stop.rs` | Tab-order groups |
| `taffy.rs` | Taffy 0.13 layout engine wrapper (`request_layout`, `compute_layout`) |
| `test.rs` | `run_test`, seed calculation, `observe()` stream helper |
| `text_system.rs`, `text_system/` | `TextSystem`, `WindowTextSystem`, `LineLayout`, `LineWrapper`, font fallbacks/features |
| `view.rs` | **`View` trait**, `ViewElement`, `AnyView`, `AnyWeakView` |
| `window.rs` (7.6k lines) | `Window`, draw pipeline, focus, hitboxes, element state, prompts, a11y |

Docs in-tree: `crates/gpui/README.md`, `crates/gpui/docs/contexts.md`,
`crates/gpui/docs/key_dispatch.md`, `crates/gpui/src/_ownership_and_data_flow.rs`,
`crates/gpui/src/_accessibility.rs` (the last two are `#[cfg(doc)]` doc-only modules).
~40 runnable examples live in `crates/gpui/examples/` — **the best idiomatic reference in the repo**.

---

## 2. Platform abstraction

### 2.1 Where the seam is

`gpui` defines the traits; `gpui_platform` is a thin `#[cfg]` dispatcher; the OS crates implement.

```rust
// crates/gpui_platform/src/gpui_platform.rs:57-81
pub fn current_platform(headless: bool) -> Rc<dyn Platform> {
    #[cfg(target_os = "macos")]   { Rc::new(gpui_macos::MacPlatform::new(headless)) }
    #[cfg(target_os = "windows")] { Rc::new(gpui_windows::WindowsPlatform::new(headless)
                                        .expect("failed to initialize Windows platform")) }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))] { gpui_linux::current_platform(headless) }
    #[cfg(target_family = "wasm")] { Rc::new(gpui_web::WebPlatform::new(true)) }
}
```

`gpui_platform` also exposes `application()`, `headless()`, `background_executor()`,
`web_init()`, `single_threaded_web()`, and (test/bench only) `current_headless_renderer()`.
**Always start apps/examples with `gpui_platform::application()`**, never `#[cfg]` yourself
(`crates/gpui/examples/hello_world.rs:90`).

### 2.2 Traits a platform must implement (`crates/gpui/src/platform.rs`)

* `Platform` (line 110) — ~70 methods: executors, `text_system()`, `run()`, `quit()`, `restart()`,
  displays, `open_window()`, URL handling, path prompts, clipboard, credentials, menus/dock menu,
  keyboard layout + `PlatformKeyboardMapper`, thermal state, system notifications, cursor.
* `PlatformWindow` (line 749) — bounds/scale/appearance, input handler, `draw(&Scene)`,
  `sprite_atlas()`, `on_*` callbacks, decorations, IME, a11y hooks.
* `PlatformDisplay` (313), `PlatformDispatcher` (967), `PlatformTextSystem` (1010),
  `PlatformAtlas`, `PlatformKeyboardMapper`, `PlatformKeyboardLayout`,
  `PlatformHeadlessRenderer` (931, test/bench only), `PlatformGestures`.

### 2.3 `#[cfg]`-gated trait methods (the ones that bite)

```
Platform::read_from_primary / write_to_primary          #[cfg(linux|freebsd)]  // PRIMARY selection
Platform::read_from_find_pasteboard / write_...         #[cfg(macos)]
PlatformWindow::get_raw_handle() -> HWND                #[cfg(windows)]  // REQUIRED, no default body
PlatformWindow::set_traffic_light_position              #[cfg(macos)]
PlatformWindow::set_exclusive_edge(layer_shell::Anchor) #[cfg(all(linux, feature="wayland"))]
PlatformWindow::as_test() / render_to_image()           #[cfg(test|test-support|bench-support)]
```

Only ~16 `target_os = "windows"` sites exist inside `crates/gpui/src` — mostly
`platform/keystroke.rs` (modifier naming/parsing), `keymap/context.rs:37` (`os = windows`
auto-context), `svg_renderer.rs:16` (emoji families = `Segoe UI Emoji`/`Segoe UI Symbol`),
`platform.rs:836` (`get_raw_handle`). Everything else lives in `gpui_windows`.

### 2.4 Per-OS crate contents

* **`gpui_windows`** — `#![cfg(target_os = "windows")]` at the crate root, so on non-Windows the
  whole crate compiles to nothing. Win32 window/message loop (`platform.rs`, `window.rs`,
  `events.rs`), `direct_write.rs` (DirectWrite text system), `directx_renderer.rs` +
  `directx_atlas.rs` + `shaders.hlsl` (D3D11 + DirectComposition), `dispatcher.rs`
  (thread-pool + `PostMessageW(WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD)`), `vsync.rs`,
  `clipboard.rs`, `destination_list.rs` (jump list), `direct_manipulation.rs` (precision
  touchpad), `system_notifications.rs`, `system_settings.rs`, `keyboard.rs`.
* **`gpui_macos`** — AppKit/Cocoa windowing, CoreText text system, pasteboard, display link.
  Depends on **`gpui_apple`**, the pure-render layer: `metal_renderer.rs`, `metal_atlas.rs`,
  `shaders.metal` (+ `cbindgen` build script). The split lets the renderer be reused without AppKit.
* **`gpui_linux`** — `src/linux/` with x11 + wayland clients; features `wayland`/`x11` each pull
  in `gpui_wgpu` (with `font-kit`).
* **`gpui_web`** — `#![cfg(target_family = "wasm")]`; one canvas, one top-level window; wgpu with
  WebGPU -> WebGL2 fallback (`WebBackendPreference`).

---

## 3. Renderers & feature flags

| Target | Renderer | Crate / files | Shader language |
|---|---|---|---|
| Windows | **DirectX 11 + DirectComposition** | `gpui_windows/src/directx_renderer.rs`, `shaders.hlsl`, `color_text_raster.hlsl`, `alpha_correction.hlsl` | HLSL (fxc) |
| macOS | Metal | `gpui_apple/src/metal_renderer.rs`, `shaders.metal` | MSL |
| Linux/FreeBSD | wgpu | `gpui_wgpu/src/wgpu_renderer.rs`, `shaders.wgsl` | WGSL |
| wasm | wgpu (WebGPU or WebGL2) | `gpui_wgpu` + `shaders_webgl.wgsl` | WGSL |

* **There is no `gpui/wgpu` feature and no blade.** `gpui_wgpu` is a *dependency* of `gpui_linux`
  (optional, enabled by `wayland`/`x11`) and of `gpui_web` only. It is **never compiled on
  Windows**. Do not pass `--features wgpu`; no such feature exists. A `grep` for blade in the tree
  hits only a worktree-name word list.
* `gpui` features (`crates/gpui/Cargo.toml`):
  `default = ["font-kit", "wayland", "x11", "windows-manifest"]`,
  `test-support` (implies `leak-detection` + `proptest`), `bench-support` (alias `bench`),
  `inspector`, `leak-detection`, `profiler`, `stacker` (stack-overflow-safe deep element trees via
  `stacksafe`, applied in `div.rs` + `taffy.rs`), `windows-manifest`.
  `font-kit`/`wayland`/`x11` are **no-ops on Windows**; `font-kit` only matters on macOS/Linux
  (README lines 27-44 spell this out).
* `gpui_wgpu` also carries `cosmic_text_system.rs` (cosmic-text + swash), the Linux/web text
  system. Not used on Windows.
* `crates/wu` features: `inspector` (pulls `gpui/inspector`), `track-project-leak` (pulls
  `gpui/leak-detection`), `test-support`, `visual-tests` (macOS-only in practice).
  `wu` depends on `gpui` with `features = ["stacker"]`.

### Windows build specifics

* `crates/gpui/build.rs` embeds `resources/windows/gpui.manifest.xml` via `embed-resource` when
  `windows-manifest` is on. `crates/wu/Cargo.toml:193` force-enables it for the `wu` binary.
* `crates/gpui_windows/build.rs` precompiles HLSL with **fxc**, but **only in release**
  (`#[cfg(not(debug_assertions))]`). Debug builds compile shaders at runtime, so a broken shader
  can pass `cargo check` and fail `--release`. `GPUI_FXC_PATH` overrides fxc discovery.
* `.cargo/config.toml` adds, for Windows only:
  `--cfg windows_slim_errors` and `-C target-feature=+crt-static`.
* Runtime escape hatch: `GPUI_DISABLE_DIRECT_COMPOSITION=1`
  (`gpui_windows/src/directx_renderer.rs:26`) when DirectComposition misbehaves.
* Lint with **`script/clippy.ps1`** on Windows (`./script/clippy` is bash). Both run
  `cargo clippy --workspace --release --all-targets --all-features -- --deny warnings`.

---

## 4. `gpui_macros` — complete export list

Source: `crates/gpui_macros/src/gpui_macros.rs`. Re-exported from `gpui` at `src/gpui.rs:110-112`:
`AppContext, IntoElement, Render, VisualContext, bench, property_test, register_action, test`.

| Macro | Kind | Notes |
|---|---|---|
| `#[derive(Action)]` | derive | See section 7. Attributes: `namespace`, `name`, `no_json`, `no_register`, `deprecated_aliases`, `deprecated`. |
| `register_action!(Ty)` | fn-like | Emits only the `inventory` registration; use when hand-implementing `Action`. |
| `#[derive(IntoElement)]` | derive | Emits `impl IntoElement { type Element = gpui::ViewElement<Self>; }`. Requires `Self: View`, auto-satisfied by any `RenderOnce`. |
| `#[derive(Render)]` | derive | `#[doc(hidden)]`. Emits a `Render` impl returning `Empty`. Test/placeholder entities only. |
| `#[derive(AppContext)]` | derive | Needs a `#[app] field: &mut gpui::App`. Compile-fails without it. |
| `#[derive(VisualContext)]` | derive | Needs **both** `#[app]` and `#[window]` fields. |
| `#[gpui::test]` | attribute | Section 5. |
| `#[gpui::bench]` | attribute | Criterion. `#[gpui::bench(inputs = ..., group = ..., input_name = ..., sample_size = ...)]`. Needs dev-deps `criterion` + `gpui_platform` (`test-support`) + `gpui/bench-support`. Pair with `gpui::bench_group!` / `gpui::bench_main!` (`src/gpui.rs:121-137`). |
| `#[gpui::property_test]` | attribute | proptest-backed. `StdRng` args are **forbidden** (they break shrinking). Use `#[strategy = 1..10]` on params. Accepts `&mut TestAppContext` / `BackgroundExecutor`. `SEED` controls only the scheduler seed. |
| `#[derive_inspector_reflection]` | attribute | On a trait; generates `<snake_trait>_reflection::{methods, find_method}`. Gated on `inspector` or `debug_assertions`. Applied to `Styled` (`styled.rs:19-23`) and skipped under `cfg(rust_analyzer)` because expansion takes ~10 s. |
| `style_helpers!`, `visibility_style_methods!`, `margin_style_methods!`, `padding_style_methods!`, `position_style_methods!`, `overflow_style_methods!`, `cursor_style_methods!`, `border_style_methods!`, `box_shadow_style_methods!` | fn-like | Generate the Tailwind-ish `Styled` methods. Invoked once, inside `pub trait Styled` (`styled.rs:27-35`). **Never call these yourself**; implement `Styled` by forwarding `fn style(&mut self) -> &mut StyleRefinement`. |

`actions!`, `bench_group!`, `bench_main!` are `macro_rules!` in `gpui` itself, not proc-macros.

---

## 5. Testing

### 5.1 `#[gpui::test]` semantics (`crates/gpui_macros/src/test.rs`)

Arguments: `seed = N`, `seeds(A, B, C)`, `iterations = N`, `retries = N`,
`on_failure = "path::to::fn"`. Env overrides: `SEED`, `ITERATIONS`.
`iterations = 5, seed = 10` is equivalent to `seeds(0,1,2,3,4,10)`. If `SEED` is set it
**overrides** explicit seeds. On multi-seed failure the harness prints `failing seed: N` plus the
`SEED=` reproduction hint (`src/test.rs:104-141`).

Accepted parameter types (anything else is a compile error, "invalid function signature"):

| Param | Sync test | Async test |
|---|---|---|
| `&mut TestAppContext` | yes (N of them) | yes (N of them) |
| `&mut App` | yes | no |
| `StdRng` | yes | yes |
| `BackgroundExecutor` | no | yes |

Generated teardown per `TestAppContext`, in this order. **This is where most flaky failures come
from:**

```rust
cx.run_until_parked();
cx.update(|cx| { cx.background_executor().forbid_parking(); cx.quit(); });
cx.run_until_parked();
drop(cx);           // dropping `_entity_refcounts` asserts NO LEAKED HANDLES
dispatcher.drain_tasks();
```

### 5.2 The context zoo

| Type | File | Availability |
|---|---|---|
| `TestAppContext` | `app/test_context.rs` | `test-support`, all platforms |
| `VisualTestContext` | `app/test_context.rs` (~line 730) | `test-support`, all platforms (uses `TestWindow`, no GPU) |
| `HeadlessAppContext` | `app/headless_app_context.rs` | `test-support`, all platforms; `capture_screenshot` needs a `PlatformHeadlessRenderer` |
| **`VisualTestAppContext`** | `app/visual_test_context.rs` | **macOS only**: `#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]` at `app.rs:72` |
| `BenchAppContext` | `app/bench_context.rs` | `bench-support` |

`VisualTestPlatform` (`platform/visual_test.rs`) is likewise macOS-only, and
`gpui_platform::current_headless_renderer()` returns `None` off macOS. Consequence:
`crates/wu/src/visual_test_runner.rs` prints "Visual test runner is only supported on macOS"
and exits 1 on Windows.

### 5.3 Key APIs

`TestAppContext`: `update`/`read`, `new`, `add_window`, `open_window(size, f)`,
`add_window_view` returning `(Entity<V>, &mut VisualTestContext)`, `add_empty_window`,
`run_until_parked`, `executor()`/`foreground_executor()`, `dispatch_action(window, action)`,
`simulate_keystrokes(window, "cmd-shift-p escape")`, `simulate_input(window, "hello")`,
`dispatch_keystroke`, `simulate_window_resize`, clipboard, `has_pending_prompt` /
`simulate_prompt_answer("Ok")`, `simulate_new_path_selection`, `simulate_path_prompt_response`,
system-notification spies, globals (`has_global`, `set_global`, `try_read_global`,
`update_global`), `notifications::<T>()` / `events::<Evt,T>()` streams,
`async fn condition(entity, predicate)`, `expect_restart()`, and `new_app()` for a second `App`
sharing the dispatcher (collaboration tests).

`VisualTestContext`: `update(|window, cx|)`, `from_window(handle, cx)`, `into_mut()`,
`dispatch_action`, `simulate_keystrokes`/`simulate_input`, `simulate_mouse_move/down/up`,
`simulate_click(position, modifiers)`, `simulate_modifiers_change`, `simulate_capslock_change`,
`simulate_resize`, `simulate_event::<E: InputEvent>`, `deactivate_window`, `simulate_close`,
`draw(space, f)`, and **`debug_bounds("SELECTOR")`** paired with
`.debug_selector(|| "SELECTOR".into())` on an element.

### 5.4 Determinism rules

* The executor is single-threaded and seeded. Background tasks **do not run** until you `.await`
  or call `run_until_parked()`.
* Awaiting anything outside the GPUI scheduler (real I/O, an OS thread, `smol::Timer`) panics with
  a parking error. Escape hatch: `cx.executor().allow_parking()`. Prefer
  `cx.background_executor().timer(dur).await` (also in root `.rules`).
* `cx.executor().advance_clock(dur)` fires timers deterministically;
  `simulate_random_delay()` injects seed-driven yields for interleaving tests.
* `GPUI_RUN_UNTIL_PARKED_LOG=1` logs what `run_until_parked` is waiting on (`executor.rs:233`).
  Use it when a test hangs.
* In tests the text system is `NoopTextSystem` (section 9), so layout assertions measure a fake
  advance of `600.0 * glyph_id`, not real font metrics.

### 5.5 Cite-worthy examples

* **`crates/gpui/examples/testing.rs`** is a guided tour: sync test, `VisualTestContext`, async +
  `run_until_parked`, `allow_parking`, `#[gpui::test(iterations = 10)]` with `StdRng`, and a mock
  distributed-system harness. **Start here.** Note its comment: "TestAppContext doesn't support
  `read(cx)`" - use `entity.read_with(cx, |t, _| ...)`.
* `crates/gpui/src/elements/anchored.rs:328-398` - `cx.open_window(size(px(800.), px(600.)), ...)`
  plus `window.rendered_frame.debug_bounds.get("MENU")`.
* `crates/gpui/src/keymap.rs:295-600` - table-driven keymap / `NoAction` / `Unbind` precedence tests.
* `crates/gpui/examples/view_example/example_tests.rs` - tests for the `View` composition model.
* Heaviest randomized users: `crates/editor/src/display_map/*` (`iterations = 100`),
  `crates/editor/src/editor_tests.rs:24656` (`iterations = 20, seeds(31)`),
  `crates/editor/src/display_map.rs:2981` (`retries = 5`). 204 files use `#[gpui::test]`.

---

## 6. Element and layout internals

### 6.1 Lifecycle

`Element` (`src/element.rs:51-142`) has three phases plus two associated state types:

```
request_layout(id, inspector_id, window, cx) -> (LayoutId, RequestLayoutState)
prepaint(id, inspector_id, bounds, &mut RequestLayoutState, window, cx) -> PrepaintState
paint(id, inspector_id, bounds, &mut RequestLayoutState, &mut PrepaintState, window, cx)
```

Plus a11y hooks: `a11y_role()`, `write_a11y_info(&mut accesskit::Node)`,
`a11y_synthetic_children(...)`. An a11y node requires **both** a non-`None` `id()` and a `Some`
`a11y_role()`.

Ordering is enforced by the `AnyElement` state machine. Calling out of order panics with
`"must call request_layout only once"`, `"must call request_layout before prepaint"`,
`"must call prepaint before paint"`, `"cannot measure after painting"`
(`element.rs:340, 453, 495, 545`). `Window` additionally has `debug_assert_paint()`,
`debug_assert_prepaint()`, `debug_assert_paint_or_prepaint()` guarding each API.

**Which phase for which `Window` API** (get this wrong and you trip a debug assert):

* prepaint-only: `insert_hitbox`, `set_focus_handle`, `set_view_id`, `defer_draw`,
  `request_autoscroll`
* paint-only: `set_key_context`, `paint_quad` / `paint_path` / `paint_glyph` / `paint_svg` /
  `paint_image` / `paint_underline` / `paint_drop_shadows`, `paint_layer`,
  `insert_window_control_hitbox`, `on_mouse_event`, `on_key_event`, `on_action`, `handle_input`
* either: `with_element_state`, `current_view`

The whole element tree plus its callbacks is **dropped every frame** and rebuilt from `Render`.
Only `with_element_state`-keyed state and entity state survive.

### 6.2 ElementId and why stability matters

The `ElementId` enum (`crates/gpui/src/window.rs:6661-6682`) has variants
`View(EntityId)`, `Integer(u64)`, `Name(SharedString)`, `Uuid(Uuid)`, `FocusHandle(FocusId)`,
`NamedInteger(SharedString, u64)`, `Path(Arc<Path>)`, `CodeLocation(core::panic::Location)`,
`NamedChild(Arc<ElementId>, SharedString)`, `OpaqueId([u8; 20])`.

`GlobalElementId` is the `Arc<[ElementId]>` path built from id-bearing ancestors. It keys element
state (hover / click / scroll / `use_state`), a11y node ids (hashed), and the inspector.

Rules:

1. An id must be **unique among the children of the first id-bearing ancestor**
   (`element.rs:60-64`).
2. An id must be **stable across frames**, or hover/scroll/animation state resets every frame.
3. `.id(x)` on any `InteractiveElement` returns `Stateful<Self>`. That is what unlocks
   `StatefulInteractiveElement`: `on_click`, `on_drag`, `tooltip`, `overflow_scroll`, `role`,
   all `aria_*`, `accessibility_id`, `a11y_synthetic_children`.
4. For list items use `ElementId::named_usize("row", ix)` or `("row", ix)`, never a bare index at
   top level.
5. `window.use_state(cx, init)` keys on the **caller source location**. Two `use_state` calls on
   the same source line, or the same line reached once per list row, collide. Use
   `use_keyed_state(key, cx, init)` there (`window.rs:3745-3785`).
6. `View::entity_id()` becomes the view element id, so two views over the same entity **must not be
   siblings** or their internal element state silently collides (`view.rs:186-191`).
7. `window.with_element_namespace(id, f)` pushes an id scope without creating an element.

### 6.3 Focus and key context

* `cx.focus_handle()` gives a `FocusHandle`; store it on the entity and implement
  `Focusable::focus_handle(&self, &App)`.
* `.track_focus(&handle)` sets `focusable = true` **and** registers the handle
  (`div.rs:757-762`). `.focusable()` only marks focusability without a handle.
* `.key_context("Editor")` or `KeyContext::parse("Editor mode = full")`. **Parse failures are
  silently swallowed and logged** (`div.rs:798-806`), so a typo means "no context", not a compile
  error.
* Tab order: `.tab_index(isize)` (implies `tab_stop(true)` and focusable), `.tab_stop(bool)`,
  `.tab_group()` (resets child indices to 0). Drive with `window.focus_next(cx)` /
  `window.focus_prev(cx)`. See `crates/gpui/examples/tab_stop.rs`.
* `window.focus(&handle)`, `blur()`, `disable_focus()`, `focused(cx)`, `focus_lost_restore_target`.
* Focus listeners on `Context<T>`: `on_focus`, `on_focus_in`, `on_blur`, `on_focus_out`,
  `on_focus_lost` (`app/context.rs:547-675`).
* `KeyContext::new_with_defaults()` pre-sets `os` to `macos` / `linux` / `windows`, so keymaps can
  write `"context": "Editor && os == windows"` (`keymap/context.rs:30-47`).

### 6.4 Hitboxes, z-order, deferral

* `window.insert_hitbox(bounds, HitboxBehavior)` during prepaint; check
  `hitbox.is_hovered(window)` during paint or in event handlers. Hit testing is topmost-wins,
  ordered by insertion (later equals on top).
* `HitboxBehavior` (`window.rs:867-900`): `Normal`, `BlockMouse` (set by `.occlude()`), and a
  scroll-permitting variant (`.block_mouse_except_scroll()`). These affect hover styles and
  tooltips, not just event routing - that is the main point of the mechanism.
* `deferred(child).with_priority(n)` lays the child out in place but paints it **after** all
  ancestors; higher `priority` draws on top. Implemented via
  `window.defer_draw(element, absolute_offset, priority, content_mask)` (prepaint-only).
* `anchored()` repositions to stay inside the window: `.snap_to_window()`, `.anchor(Corner)`,
  `.position(pt)`, `.offset(pt)`, `AnchoredFitMode`. Its children **must have no margin** or
  measurement breaks. Popovers are `deferred(anchored()...).with_priority(1)` - see
  `elements/anchored.rs:300-326` and `examples/popover.rs`.
* `window.with_content_mask(mask, f)` lets an element paint outside its bounds. "With great power,
  comes great responsibility" (`element.rs:21-26`).
* `paint_layer(bounds, f)` batches non-overlapping geometry for performance.

### 6.5 The list family

| Element | Use when | State |
|---|---|---|
| `uniform_list(id, count, render_range)` | all rows the same height | `UniformListScrollHandle` via `.track_scroll(&h)`. Measures item 0 and multiplies; bypasses Taffy per row. |
| `list(ListState, render_item)` | variable heights, huge counts | `ListState` (an `Rc<RefCell<..>>` sum-tree). **You must call `ListState::splice` / `reset` when item heights change**, or off-screen measurements go stale (module doc, `elements/list.rs:1-8`). |
| `canvas(prepaint_fn, paint_fn)` | ad-hoc low-level drawing | no id, no state |
| `container_query(render_with_size)` | contents depend on measured size | contents cannot influence the size; defaults to `size_full()` |

`ScrollHandle` (`div.rs:4055+`): `offset()`, `set_offset()`, `max_offset()`, `bounds_for_item(ix)`,
`scroll_to_item`, `scroll_to_top_of_item`, `scroll_to_bottom`, `logical_scroll_top` /
`logical_scroll_bottom`, `children_count`. `ScrollAnchor::for_handle(..)` pins scroll position
across content changes. `UniformListScrollHandle` adds `scroll_to_item_strict` (and
`_with_offset` variants), `y_flipped`, `is_scrolled_to_end`, `logical_scroll_top_index`.

`Div` extras worth knowing: `.on_children_prepainted(cb)` (receives the child bounds vector after
prepaint), `.with_dynamic_prepaint_order(order_fn)` (needed when one child prepaint feeds another,
e.g. split editors and autoscroll), `.image_cache(provider)`.

### 6.6 View and ViewElement (fork-specific, read this)

`crates/gpui/src/view.rs:182-192` defines `trait View { fn entity_id(&self) -> Option<EntityId>;
fn render(self, window, cx) -> impl IntoElement; }` with two blanket impls: every `RenderOnce`
becomes a `View` with `entity_id() == None` (stateless component), and every `Entity<T: Render>`
becomes a `View` keyed on its own id (identity plus subtree-scoped notify).

* `.child(some_entity)` works directly; no `.into_any_element()` needed.
* `entity.cached(style)` reuses the rendered subtree until the entity is notified. **It requires a
  definite size from `style`**, because a cached view is never measured from its contents
  (`view.rs:222-236`).
* Implement `View` by hand only when a component needs both parent-supplied props and a backing
  entity for identity.

---

## 7. Actions and keymaps at the GPUI level

### 7.1 Declaring

```rust
// unit actions; doc comments are surfaced to users via Action::documentation()
actions!(editor, [/** Moves the cursor up. */ MoveUp, MoveDown]);

// data-carrying actions
#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = editor, deprecated_aliases = ["editor::SelectNextOccurrence"])]
pub struct SelectNext { pub replace_newest: bool }
```

`#[action(...)]` options (`gpui_macros/src/derive_action.rs:26-118`):

* `namespace = ident` - required by convention in this repo; produces `namespace::Name`.
* `name = "Str"` - overrides the type name. Must not contain `::` (panics at compile time).
* `no_json` - `build` always errors, `action_json_schema` returns `None`, and the type no longer
  needs `serde::Deserialize` / `schemars::JsonSchema`.
* `no_register` - implements `Action` without registering it (no invocation by name, no JSON).
* `deprecated_aliases = ["old::Name"]` - old names still resolve; must not collide with a real
  registered action.
* `deprecated = "message"` - surfaces as a keymap schema warning.

The derive requires `Clone` and `PartialEq`; unit structs skip JSON entirely.
`actions!(ns, [A, B])` expands to `#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]`
plus `#[action(namespace = ns)]` on each unit struct. The namespace argument may be omitted, but
Zed/Wu actions require one.

### 7.2 Registration mechanism

`register_action!` and the derive emit an `inventory::submit!` of a `MacroActionBuilder`
(`gpui_macros/src/register_action.rs:15-48`). `ActionRegistry::default()` walks
`inventory::iter::<MacroActionBuilder>` when the `App` is created (`action.rs:284-291`).
Therefore:

* Registration is **link-time global**. An action in a crate that is not linked does not exist.
* Duplicate names **panic at `App` creation**, not at compile time:
  "Action with name `X` already registered (might be registered in
  `#[action(deprecated_aliases = [...])]`."
* Deprecated aliases are inserted into the same `by_name` map, so an alias colliding with a real
  action also panics.
* `generate_list_of_all_registered_actions()` powers keymap-schema and docs generation.

### 7.3 Keymap resolution

A JSON entry resolves as `ActionRegistry::build_action(name, params)` -> `Box<dyn Action>`;
params default to `{}`. Unit structs ignore the JSON; others go through
`serde_json::from_value`. Failures surface as `ActionBuildError::{NotFound, BuildError}`
(a dedicated enum, not `anyhow`, so Zed can render it as markdown).

Matching (`Keymap::bindings_for_input`, `keymap.rs:165-240`):

1. Iterate bindings **in reverse insertion order**, so later wins at equal depth. The user keymap
   is loaded after the defaults.
2. `binding_enabled` calls `predicate.depth_of(context_stack)`. A binding with **no** context
   predicate is treated as matching at the *deepest* context.
3. Sort by depth descending, then by index descending.
4. `wu::NoAction` suppresses lower-ranked matches **from sources with equal or weaker precedence**,
   tracked via `binding.meta`; a user binding still beats a base-keymap `null`.
5. `wu::Unbind("editor::NewLine")` - written in JSON as `["wu::Unbind", "editor::NewLine"]` -
   removes bindings that dispatch that specific action for the same keystrokes, regardless of
   context.
6. Returns `(bindings, has_pending)`. `has_pending` drives multi-key sequences such as
   `"cmd-k left"`.

`KeyContext` syntax: a bare identifier (`StatusBar`), a key/value pair (`mode = visible`), or
several separated by whitespace (`StatusBar mode = visible`) (`keymap/context.rs:60-68`).

### 7.4 Dispatching and handling

* Element-level: `.on_action(cx.listener(Editor::undo))` (bubble),
  `.capture_action(...)` (capture), `.on_boxed_action(&boxed, ...)`.
* Imperative: `window.dispatch_action(action.boxed_clone(), cx)`,
  `focus_handle.dispatch_action(&Increment, window, cx)`, `cx.on_action` on a `Context<T>`,
  `window.on_action` / `window.on_action_when`.
* Introspection: `window.available_actions()`, `is_action_available[_in]`,
  `bindings_for_action[_in][_in_context]`, `highest_precedence_binding_for_action*`,
  `possible_bindings_for_input`, `context_stack()`, `has_pending_keystrokes()`,
  `pending_input_keystrokes()`, `keystroke_text_for(action)`.
* **Action handlers stop propagation by default during the bubble phase.** Call `cx.propagate()`
  to keep bubbling; `cx.stop_propagation()` is the opposite and is the default for actions
  (`app.rs:2273-2280`).
* Binding keys in code: `cx.bind_keys([KeyBinding::new("up", Increment, Some("Counter"))])`
  (`examples/testing.rs:181-184`).

---

## 8. Globals

```
cx.global::<G>()        // &G   - PANICS "no state of type G exists"
cx.global_mut::<G>()    // &mut G - panics too; also queues NotifyGlobalObservers
cx.try_global::<G>()    // Option<&G>
cx.has_global::<G>()
cx.default_global::<G>()          // G: Default, inserts if missing
cx.set_global(g); cx.remove_global::<G>();   // remove panics if absent
cx.update_global::<G, _>(|g, cx| ...)        // via BorrowAppContext
cx.update_default_global::<G, _>(|g, cx| ...)
cx.observe_global::<G>(|cx| ...) -> Subscription
```

`Global` is a bare marker trait (`src/global.rs:22`). Blanket traits `ReadGlobal::global(cx)` and
`UpdateGlobal::{update_global, set_global}` let you write `MySettings::global(cx)`.

**`update_global` leases the global out of the map.** `App::lease_global` *removes* the box, runs
the closure, then `end_global_lease` puts it back (`gpui.rs:322-331`, `app.rs:2096-2110`). So
inside `cx.update_global::<G, _>(...)`, a nested `cx.global::<G>()` panics with
"no state of type G exists" - the same shape as the entity double-lease panic.

When a `Global` is the **wrong** tool:

* You want change notifications scoped to a subtree: use an `Entity<T>` plus `cx.observe`.
  `observe_global` fires app-wide on every mutation, including unrelated ones.
* Per-window state: use `Window` fields or `window.use_state`.
* Reading a global does **not** subscribe you to it. Entities that render from a global must call
  `cx.observe_global::<G>()` explicitly or they will render stale data.
* Restricting access: the idiomatic pattern is a private `struct GlobalX(X); impl Global for
  GlobalX {}` plus newtype accessors (documented at `global.rs:12-21`). In-tree examples:
  `GroupHitboxes` (`div.rs:3848`) and `GlobalTokio` (`gpui_tokio`).
* `Global` is `'static` and not `Send`; it lives on the main thread only.
* Tests: each `TestAppContext` owns its own `App`, so globals do not leak between tests.
  `cx.clear_globals()` exists under `test-support`.

---

## 9. Assets, images, SVG, fonts, text

* `AssetSource` (`assets.rs:13-19`): `load(&str) -> Result<Option<Cow<[u8]>>>` (static lifetime on
  the `Cow`) and `list(&str) -> Result<Vec<SharedString>>`. The unit type `()` implements it as a
  no-op. Wire it up with `Application::with_assets(...)`; read it back via `cx.asset_source()`.
* `Asset` trait (`asset_cache.rs:38-51`): `type Source: Clone + Hash + Send`,
  `type Output: Clone + Send`, `async fn load(source, cx)`. Drive it from an element with
  `window.use_asset::<A>(&source, cx) -> Option<A::Output>`, which returns `None` and re-renders
  when the value arrives, or `window.get_asset::<A>()` for a non-scheduling peek.
  `AssetLogger<T>` wraps a fallible asset and logs the error variant.
* `Resource::{Uri, Path, Embedded}` selects the loading strategy. `img(source)` accepts
  `SharedUri`, `PathBuf`, `Arc<Path>`, `SharedString` (embedded asset), `Arc<RenderImage>`,
  `Arc<Image>`, or `ImageSource::Custom(fn)`. Animated GIF and WebP are supported.
  `LOADING_DELAY` is 200 ms before the loading state shows (`elements/img.rs:31`).
* `svg()` accepts `.path("icons/x.svg")` (asset source), `.external_path(..)` (filesystem), or
  `.data(&[u8])` (hashed into a synthetic `__binary_svg__<hash>` cache key). Rasterized by
  resvg/usvg in `svg_renderer.rs`; text inside SVGs resolves emoji per OS
  (Windows: `Segoe UI Emoji`, `Segoe UI Symbol`).
* `.image_cache(provider)` on a `Div` scopes a retained image cache to a subtree
  (`elements/image_cache.rs`).
* Text: `TextSystem` is app-wide (font ids, metrics, fallbacks, `add_fonts`, `resolve_font`,
  `line_wrapper`); `WindowTextSystem` is the per-window shaping cache (`shape_line`, `shape_text`,
  `layout_line`, `layout_width`, `em_advance`, `ch_width`). Build fonts with
  `font("Zed Mono").bold().italic()`, plus `FontFeatures`, `TextRun`, `ShapedLine`, `WrappedLine`.
  Register embedded fonts with `cx.text_system().add_fonts(vec![...])`.
* **In tests the platform text system is `NoopTextSystem`** (`platform/test/platform.rs:82`,
  defined at `platform.rs:1046`): `font_id` is always `FontId(1)`, advance is
  `600.0 * glyph_id`, and nothing rasterizes. Layout-sensitive assertions under `#[gpui::test]`
  measure that synthetic metric, not real fonts.
* Windows text is DirectWrite (`gpui_windows/src/direct_write.rs`). The gamma/contrast env knobs
  `ZED_FONTS_GAMMA`, `ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST`,
  `ZED_FONTS_SUBPIXEL_ENHANCED_CONTRAST` are read by the **wgpu** renderer only
  (`gpui_wgpu/src/wgpu_renderer.rs:2174-2190`), so they do not apply on Windows.
* `elements/surface.rs` (`SurfaceSource`) is macOS-only; it wraps a CoreVideo `CVPixelBuffer`.

---

## 10. Footguns and panics

### 10.1 Entity borrow errors (beyond "do not update twice")

* `EntityMap::lease` removes the entity from the slotmap. Any nested access while leased panics:
  "cannot update `<T>` while it is already being updated" or "cannot read `<T>` while it is already
  being updated" (`app/entity_map.rs:207-212`). **`entity.read(cx)` panics too**, not just
  `update` - a common surprise. Reading the entity you are currently inside is the classic trigger.
* `Lease` has a `Drop` guard: "Leases must be ended with EntityMap::end_lease". Seeing this means a
  panic escaped an update closure.
* `debug_assert!` "used a entity with the wrong context" fires when an `Entity` from one `App`
  (say `cx_a`) is used with another (`cx_b`) in a multi-context test.
* "detected over-release of a entity" / "Detected over-release of a handle." indicate refcount
  corruption, usually cross-`App` handle mixing.
* Globals have the same shape: `cx.global::<G>()` inside `cx.update_global::<G, _>()` panics
  (section 8).
* The fix for all of these is `cx.defer(...)` or `cx.defer_in(window, ...)`, which runs the closure
  at the end of the current effect cycle when the entity is back in the map.

### 10.2 Tasks and async

* A dropped `Task` cancels its future **silently**. Use `.detach()`, `.detach_and_log_err(cx)`
  (from `gpui::TaskExt` - the trait must be imported), store it in a field, or await it.
* Never hold `&mut App`, `&mut Context<T>` or `&mut Window` across an `.await`; they are borrowed,
  not owned. Convert first: `cx.to_async()` gives `AsyncApp`, `window.to_async(cx)` gives
  `AsyncWindowContext`. Async context methods return `Result`.
* `AsyncApp::as_mut` panics on purpose: "Cannot as_mut with an async context. Try calling update()
  first" (`async_context.rs:73` and `:436`).
* Upgrade failure message: "app was released before async operation completed"
  (`async_context.rs:32`).
* `WeakEntity::update` / `read_with` / `update_in` return `Result`. A bare `.unwrap()` is a latent
  crash when the view closes mid-task. Prefer `.ok()`, which is exactly what `cx.listener` does
  (`app/context.rs:255-260`).
* "Can not spawn on main thread after on_app_quit" (`app.rs:1943`, `1964`, `1982`) covers
  foreground spawns during shutdown.
* `background_executor().spawn` polls on platform worker threads. On macOS those are GCD workers
  with **512 KiB stacks**; deeply recursive futures need `spawn_dedicated(...)` (2 MiB)
  (`executor.rs:91-110`).
* Task/entity cycles: an entity holding a `Task` that holds a strong `Entity` back to itself never
  drops, and the `#[gpui::test]` teardown fails with "Handles for `<T>` leaked". Break the cycle
  with `WeakEntity`. Debug with `LEAK_BACKTRACE=1 cargo test my_test`
  (`app/entity_map.rs:632-640`, `998-1024`).
* `gpui_tokio::Tokio::spawn` cancels the Tokio task when the returned GPUI `Task` drops.

### 10.3 Rendering and element phase

* Phase violations: "must call request_layout only once", "must call request_layout before
  prepaint", "must call prepaint before paint", "cannot measure after painting"
  (`element.rs:340/453/495/545`). Usually caused by measuring an `AnyElement` that was already
  painted, or taking a child out of an `Option` twice.
* "reentrant call to with_element_state for the same state type and element id"
  (`window.rs:3838`) - nested `with_element_state` with the same `(GlobalElementId, TypeId)`.
* "invalid element state type for id, requested X, actual Y" - the same `ElementId` reused by two
  elements that store different state types.
* "you must return some state when you pass some element id" (`window.rs:3886`) plus the mirror
  `debug_assert` for the `None` case in `with_optional_element_state`.
* `Interactivity` builders `debug_assert` on double-set: "hover style already set" when
  `.hover(...)` is called twice on one element. Same idea for `role != GenericContainer`.
* "Re-entrant window prompting is not supported by GPUI" (`window.rs:5801`) - a second
  `window.prompt()` while one is open.
* a11y invariants panic loudly: "set_focus called more than once in a single frame",
  "active descendant claimed by multiple nodes in one frame", "set_focus called for a node that
  was not registered with set_focusable", "active_descendant set to X, which is not in the tree"
  (`window/a11y.rs:232, 259, 515, 529, 564`).
* Deep element trees can overflow the stack; the `stacker` feature (`stacksafe`) guards
  `Div::{request_layout, prepaint, paint}` and `taffy.rs`. The `wu` binary enables it.
* `.debug_selector(..)` is a **no-op outside test builds** (`div.rs:849-857`); never rely on it in
  production paths.
* `Text` panics on malformed runs: "invalid text run. Text: ..., run: ..."
  (`elements/text.rs:531`) when `TextRun` lengths do not sum to the string length.

### 10.4 Missing cx.notify and stale UI

* Mutating entity state without `cx.notify()` leaves the view stale; rendering does not diff.
* `window.use_keyed_state` auto-wires `cx.observe(&state, |_, cx| cx.notify(current_view))`
  (`window.rs:3759-3762`), so hooks re-render for free while plain fields do not.
* `entity.cached(style)` only re-renders on notify, or when cached bounds or text style change. A
  cached view that mutates without notifying never repaints.
* `cx.global_mut::<G>()`, `set_global`, `update_global` and `remove_global` all queue
  `NotifyGlobalObservers`, but entities that merely read a global are **not** observers.
* `window.refresh()` forces a full-window redraw. `window.request_animation_frame()` schedules one
  frame; respect `App::reduce_motion` for decorative motion and prefer
  `AnimationExt::with_animation`.

### 10.5 Subscriptions

* A `Subscription` **unsubscribes on drop**. `let _ = cx.subscribe(...)` drops it immediately
  (and violates root `.rules`). Store it in a `_subscriptions: Vec<Subscription>` field or
  `.detach()`.
* Subscriptions are created **inert** and activated through a deferred effect
  (`subscription.rs:44-50`; `App::observe_global` calls `self.defer(move |_| activate())`). In a
  test, a subscription registered and fired in the same synchronous block may not have activated -
  call `cx.run_until_parked()` first.
* Long-lived `.detach()`ed subscriptions that capture strong `Entity` handles are a leak source;
  capture `WeakEntity` instead.

### 10.6 defer, on_next_frame, on_drop

* `cx.defer(f)`, `window.defer(cx, f)` and `cx.defer_in(window, f)` run at the **end of the current
  effect cycle**. `defer_in` re-resolves the window and silently `.ok()`s if the view is gone
  (`app/context.rs:305-317`).
* `window.on_next_frame(f)` runs **after the next frame is rendered**. It wakes the platform frame
  source explicitly but does **not** dirty the window (`window.rs:2351-2359`). In tests there is no
  frame loop, so call `window.simulate_next_frame(cx)`.
* `window.request_animation_frame()` is `on_next_frame(|_, cx| cx.notify(current_view))`. Because
  it captures `current_view()`, it **must be called during prepaint or paint**.
* `Context::on_drop(f)` returns a `gpui_util::Deferred`; hold it, and dropping it runs the closure.

---

## 11. Windows-specific notes (this repo is worked on from Windows)

### Works fine on Windows

* Everything in `crates/gpui/src` except the macOS-gated test contexts.
* `#[gpui::test]`, `TestAppContext`, `VisualTestContext`, `TestPlatform` / `TestWindow`,
  `HeadlessAppContext`, `#[gpui::property_test]`, `#[gpui::bench]`.
* `cargo test -p gpui` and the 200+ `#[gpui::test]` suites across the workspace.

### Does NOT work / cannot be tested on Windows

| Thing | Why |
|---|---|
| `VisualTestAppContext` | `#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]` at `app.rs:72` |
| `VisualTestPlatform` | `#[cfg(all(target_os = "macos", ...))]` at `platform.rs:79` |
| `wu_visual_test_runner`, `--features visual-tests` | stub `main` prints "Visual test runner is only supported on macOS" and exits 1 (`crates/wu/src/visual_test_runner.rs:38-42`) |
| `capture_screenshot` on `HeadlessAppContext` | `gpui_platform::current_headless_renderer()` returns `None` off macOS |
| `gpui_macos`, `gpui_apple`, `gpui_linux`, `gpui_wgpu`, `gpui_web` | not in the dependency graph for a Windows build (target-gated in `gpui_platform/Cargo.toml`). Edits there are **compile-unverifiable locally**: read carefully, keep changes minimal, and say so in the PR. |
| `Platform::read_from_primary` / `write_to_primary` (Linux), `read_from_find_pasteboard` (macOS), `set_traffic_light_position`, `set_exclusive_edge` | `#[cfg]`-gated away |
| `runtime_shaders` feature | macOS / Metal only |
| `elements/surface.rs` `SurfaceSource` | CoreVideo, macOS only |

### Windows-only API surface

`fn get_raw_handle(&self) -> windows::Win32::Foundation::HWND;` is `#[cfg(target_os = "windows")]`
with **no default body** (`platform.rs:836-838`). Any new `PlatformWindow` implementation must
provide it under that cfg, and it will look unused when you read the trait on another OS.

### Build and lint on Windows

* Use `script/clippy.ps1` (not `script/clippy`, which is bash). Also available:
  `script/bootstrap.ps1`, `script/bundle-windows.ps1`, `script/install-rustup.ps1`,
  `script/generate-licenses.ps1`, `script/get-crate-version.ps1`.
* `.cargo/config.toml` injects `--cfg windows_slim_errors` and `-C target-feature=+crt-static`
  for `cfg(target_os = "windows")`. Note that target-specific `rustflags` **replace**
  `build.rustflags` rather than adding to them, so global flags must be repeated.
* HLSL is precompiled by `gpui_windows/build.rs` **only in release**, so a broken shader can pass
  `cargo check` (debug) and fail `--release`. `GPUI_FXC_PATH` overrides fxc discovery.
* `crates/gpui` itself depends on the `windows` crate on this target
  (`Win32_Foundation`, `Win32_System_Power`).

### Windows runtime behaviour worth knowing

* Foreground tasks are delivered by `PostMessageW(hwnd, WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD)`
  into the Win32 message loop, coalesced by a `wake_posted: AtomicBool`
  (`gpui_windows/src/dispatcher.rs`). Background work uses `TrySubmitThreadpoolCallback` with
  `TP_CALLBACK_PRIORITY_{HIGH,NORMAL,LOW}` mapped from `gpui::Priority`, plus
  `timeBeginPeriod` / `timeEndPeriod` behind `TimerResolutionGuard`.
* Internal window messages: `WM_GPUI_CLOSE_ONE_WINDOW`, `WM_GPUI_KEYDOWN`,
  `WM_GPUI_CURSOR_STYLE_CHANGED`, `WM_GPUI_DOCK_MENU_ACTION`, `WM_GPUI_KEYBOARD_LAYOUT_CHANGED`,
  `WM_GPUI_GPU_DEVICE_LOST`, `WM_GPUI_END_SESSION`.
* GPU device-lost is handled explicitly: `DirectXRenderer::skip_draws` discards the first frame
  after a reset because all GPU textures and scene resources were lost mid-frame. Do not simplify
  that away.
* App restart shells out to PowerShell with `ZED_RESTART_PID`, `ZED_RESTART_EXECUTABLE`,
  `ZED_RESTART_ARGUMENTS` (`gpui_windows/src/platform.rs:490-530`).
* Keystroke text differs on Windows (`platform/keystroke.rs` has about a dozen windows cfgs).
  Never hard-code `"cmd-"` in cross-platform code; use `Keystroke::parse` and the keymap
  `os == windows` context.
* Use `gpui_util::new_std_command(program)` instead of `std::process::Command::new` for anything
  user-facing: it sets `CREATE_NO_WINDOW` (`0x0800_0000`) on Windows so no console flashes
  (`gpui_util/src/lib.rs:17-27`).

---

## 12. Companion crates

* **`gpui_util`** - `ResultExt::log_err`, `TryFutureExt`, `defer()`, `post_inc`, `ArcCow`,
  `FutureExt::with_timeout` / `Timeout`, and `new_std_command()` (see above). `ArcCow` and
  `FutureExt`/`Timeout` are re-exported from `gpui`.
* **`gpui_shared_string`** - `SharedString`, backed by `smol_str`, with serde and schemars impls.
  Re-exported wholesale by `gpui`. Cheap to clone; prefer it over `String` in element and props
  types.
* **`gpui_tokio`** - `gpui_tokio::init(cx)` builds a 2-worker Tokio runtime, or
  `init_from_handle(cx, handle)` reuses an existing one. `Tokio::spawn(cx, fut)` returns
  `Task<Result<R, JoinError>>`. Stored as a private `GlobalTokio` whose `Drop` calls
  `shutdown_background()`.
* **`scheduler`** (outside this domain but load-bearing) - owns `Task`, `Priority`,
  `BackgroundExecutor`, `LocalExecutor`, `TestScheduler`, `Clock`, `Timer`. `gpui` re-exports
  the parts you use.

---

## 13. Quick reference: where to look

| Question | File |
|---|---|
| How do I write a GPUI test? | `crates/gpui/examples/testing.rs` |
| How do I compose views? | `crates/gpui/examples/view_example/` |
| How does text input work? | `crates/gpui/examples/input.rs`, `crates/gpui/src/input.rs` |
| Popover / menu positioning | `crates/gpui/examples/popover.rs`, `src/elements/anchored.rs` |
| Virtualized lists | `crates/gpui/examples/uniform_list.rs`, `list_example.rs`, `data_table.rs` |
| Drag and drop | `crates/gpui/examples/drag_drop.rs` |
| Animation | `crates/gpui/examples/animation.rs`, `src/elements/animation.rs`, `src/spring.rs` |
| Accessibility | `crates/gpui/src/_accessibility.rs`, `examples/a11y.rs`, `src/window/a11y.rs` |
| Ownership / data-flow narrative | `crates/gpui/src/_ownership_and_data_flow.rs`, `examples/ownership_post.rs` |
| Key dispatch model | `crates/gpui/docs/key_dispatch.md`, `src/key_dispatch.rs:1-51` |
| Context types | `crates/gpui/docs/contexts.md` |
| Keymap precedence semantics | `crates/gpui/src/keymap.rs:150-240`, tests at `:295-600` |
| Window menus / dock menu | `crates/gpui/examples/set_menus.rs`, `src/platform/app_menu.rs` |
| Custom painting | `crates/gpui/examples/painting.rs`, `gradient.rs`, `shadow.rs`, `src/path_builder.rs` |
| Multi-window entity moves | `crates/gpui/examples/move_entity_between_windows.rs` |
