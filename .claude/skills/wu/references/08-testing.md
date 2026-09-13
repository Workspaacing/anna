# Anna — Testing, Test Infrastructure & Performance Measurement

Repo: `C:/Users/USER/Documents/wu-main` (Rust, built on Zed). Read-only survey.
Toolchain pinned: `rust-toolchain.toml` -> **1.97.1** (stable, minimal profile).

Scale of the test suite (measured):

| thing | count |
|---|---|
| `#[gpui::test]` call sites | 3004 |
| plain `#[test]` | 1591 |
| `#[cfg(test)] mod tests { … }` (inline) | 329 |
| `#[cfg(test)] mod test { … }` (inline, singular) | 17 |
| `#[cfg(test)] mod <name>;` (sibling file) | 32 |
| `#[gpui::property_test]` | 9 |
| `#[perf]` | 84 |
| `#[gpui::bench]` | 14 |
| `FakeFs::new` uses | 1091 |
| `EditorTestContext::new` uses | 236 |
| `EditorLspTestContext::new*` uses | 164 |
| `#[ignore]` | 15 |

---

## 1. Test layout conventions

### 1.1 The dominant convention: inline `#[cfg(test)] mod tests`

**~90% of test modules are inline at the bottom of the source file** they test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_os_release() { /* … */ }
}
```
Real example: `C:/Users/USER/Documents/wu-main/crates/util/src/util.rs:794` (`mod tests` at the very bottom, `use super::*;` first line). Same shape in `crates/git/src/git.rs:369`, `crates/rope/src/rope.rs:1728`, `crates/sum_tree/src/sum_tree.rs:1417`, `crates/workspace/src/workspace.rs:10016`.

A minority use singular `mod test { … }` (17 sites). Don't invent a third name.

### 1.2 Second convention: sibling `*_tests.rs` file, declared with `#[cfg(test)]`

When the test module gets big it moves to a sibling file next to the source file — **never** into a `mod.rs`. From `C:/Users/USER/Documents/wu-main/crates/editor/src/editor.rs:48-56`:

```rust
#[cfg(test)]
mod code_completion_tests;
#[cfg(test)]
mod editor_block_comment_tests;
#[cfg(test)]
mod editor_tests;
mod signature_help;
#[cfg(any(test, feature = "test-support"))]
pub mod test;
```

Note the two different gates:
- `#[cfg(test)]` for the *tests themselves* (`editor_tests`),
- `#[cfg(any(test, feature = "test-support"))]` for *helpers other crates need* (`pub mod test`).

Other examples of the sibling-file style:
- `crates/language/src/language.rs:27-30` -> `buffer_tests`, `proto_diagnostics_tests`
- `crates/language/src/syntax_map.rs:1-2` -> `syntax_map_tests`
- `crates/multi_buffer/src/multi_buffer.rs` -> `multi_buffer_tests`
- `crates/workspace/src/workspace.rs:9-10` -> `multi_workspace_tests`
- `crates/text/src/text.rs` -> `tests`
- `crates/diagnostics`, `crates/file_finder`, `crates/tab_switcher`, `crates/open_path_prompt`, `crates/language_tools`, `crates/extension_host` — all `<crate>_tests.rs` / `<thing>_test.rs`.

### 1.3 The `mod.rs` prohibition and how test *directories* work

Root `.rules`:
> `* Never create files with `mod.rs` paths - prefer `src/some_module.rs` instead of `src/some_module/mod.rs`.`

So a test module with submodules is `src/tests.rs` **plus** a `src/tests/` directory:
- `crates/project_panel/src/project_panel.rs:7939-7941` declares `#[cfg(test)] mod project_panel_tests;` and `mod tests;`; `crates/project_panel/src/tests.rs` contains only `pub(crate) mod undo;` and `crates/project_panel/src/tests/undo.rs` holds the tests.
- `crates/debugger_ui/src/debugger_ui.rs:24-25` declares `#[cfg(any(test, feature = "test-support"))] pub mod tests;`; `crates/debugger_ui/src/tests.rs` holds `init_test`/`init_test_workspace` helpers plus 11 `#[cfg(test)] mod …;` declarations for `crates/debugger_ui/src/tests/*.rs`.
- `crates/editor/src/test.rs` + `crates/editor/src/test/{editor_test_context,editor_lsp_test_context}.rs`.
- `crates/util/src/test.rs` + `crates/util/src/test/{assertions,marked_text}.rs`.

Same rule applies to lib roots: `[lib] path = "src/<crate>.rs"` everywhere (not `lib.rs`).

### 1.4 The exception: `tests/` integration directories (only 6 crates)

`crates/{fs,gpui,gpui_macros,project,settings_content,worktree}/tests`.

For `fs`, `project`, `worktree` this is a deliberate build-time optimization: the **lib target has `test = false`**, and all tests live in one integration binary declared explicitly. From `C:/Users/USER/Documents/wu-main/crates/project/Cargo.toml:11-19`:

```toml
[lib]
path = "src/project.rs"
doctest = false
test = false

[[test]]
name = "integration"
required-features = ["test-support"]
path = "tests/integration/project_tests.rs"
```

`tests/integration/project_tests.rs` is the *root* of one binary and starts with `mod bookmark_store; mod color_extractor; mod debugger; …` (17 submodules). Same shape in `crates/fs/tests/integration/fs_tests.rs` (`mod fake_git_repo_tests;`) and `crates/worktree/tests/integration/worktree_tests.rs` (`mod worktree_settings_tests;`).

**Rule for an agent:** put a new test next to the code (inline `mod tests` or a `*_tests.rs` sibling). Only add to `tests/integration/` if you are editing `fs`, `project`, or `worktree` — those crates' lib targets cannot host tests at all.

### 1.5 Logger bootstrap

Many test modules install the logger once via a constructor:

```rust
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn init_logger() {
    zlog::init_test();
}
```
(`crates/text/src/tests.rs:11-15`, `crates/language/src/buffer_tests.rs:40-44`, `crates/editor/src/test.rs:22-26`, `crates/rope/src/rope.rs:1734`, `crates/multi_buffer/src/multi_buffer_tests.rs:16`, and 13 more.)

`zlog::init_test()` (`crates/zlog/src/zlog.rs:27`) is a **no-op unless `ZED_LOG` / `RUST_LOG` is set, or `CI` is set** (then defaults to `info`). So `log::info!` in a randomized test is free in normal runs and becomes a trace when you set `ZED_LOG=info`.

---

## 2. The GPUI test harness — `#[gpui::test]`

- Macro implementation: `C:/Users/USER/Documents/wu-main/crates/gpui_macros/src/test.rs`
- Runtime: `C:/Users/USER/Documents/wu-main/crates/gpui/src/test.rs` (`gpui::run_test`)
- Attribute reference doc comment: `C:/Users/USER/Documents/wu-main/crates/gpui_macros/src/gpui_macros.rs:151-188`
- Executor test helpers: `C:/Users/USER/Documents/wu-main/crates/gpui/src/executor.rs:174-260`
- Deterministic scheduler: `C:/Users/USER/Documents/wu-main/crates/scheduler/src/test_scheduler.rs`
- **Read `C:/Users/USER/Documents/wu-main/crates/gpui/examples/testing.rs` — a hand-written, commented tour of every pattern below.**

### 2.1 Exact supported attribute arguments

Parsed in `crates/gpui_macros/src/test.rs:20-93`. Anything else is a compile error ("invalid argument name"):

| form | meaning |
|---|---|
| `#[gpui::test]` | one run, seed `0` (or `$SEED` if set) |
| `#[gpui::test(seed = 10)]` | one run with seed 10 |
| `#[gpui::test(seeds(10, 20, 30))]` | three runs, those seeds |
| `#[gpui::test(iterations = 100)]` | 100 runs with seeds `0..100` |
| `#[gpui::test(iterations = 20, seeds(31))]` | seeds `0..20` **plus** 31 |
| `#[gpui::test(retries = 5)]` | re-run up to 5 extra times on panic before failing |
| `#[gpui::test(on_failure = "crate::test::report_failure")]` | call that path (string literal) after final failure |

Combinations are additive. `seed`/`seeds` are ignored entirely when `$SEED` is set (`crates/gpui/src/test.rs:177-182`).

In-tree usage: `iterations = 100` (33x), `iterations = 10` (30x), `iterations = 50` (14x), `retries = 5` (4x), pinned regression seeds like `seeds(340, 472)`.

### 2.2 Exact supported *parameter* types

The macro inspects the fn signature by **type name of the last path segment** — order is free, count is free:

| parameter type | what you get |
|---|---|
| `cx: &mut TestAppContext` | a fresh `TestAppContext`. Repeat it for multi-client tests: `(cx_a: &mut TestAppContext, cx_b: &mut TestAppContext)` — **both share one `TestDispatcher`**, so `run_until_parked` interleaves them pseudo-randomly per seed. |
| `cx: &mut App` | **sync tests only.** A borrowed `&mut App`. |
| `rng: StdRng` | `rand::SeedableRng::seed_from_u64(seed)` |
| `executor: BackgroundExecutor` | `BackgroundExecutor::new(Arc::new(dispatcher.clone()))` |

Anything else produces `compile_error!("invalid function signature")`.

`async fn` is supported; the body is driven by `ForegroundExecutor::block_test(...)`. `&mut App` is **not** allowed in async tests (only `&mut TestAppContext`).

### 2.3 What the macro generates (and why teardown matters)

For each seed the macro emits (`crates/gpui_macros/src/test.rs:185-210`):

```rust
gpui::run_test(num_iterations, &[seeds], max_retries, &mut |dispatcher, _seed| {
    let exec = std::sync::Arc::new(dispatcher.clone());
    let mut cx_0 = gpui::TestAppContext::build(dispatcher.clone(), Some("test_name"));
    let _entity_refcounts = cx_0.app.borrow().ref_counts_drop_handle();
    gpui::ForegroundExecutor::new(exec.clone()).block_test(__test_name(&mut cx_0));
    drop(exec);
    cx_0.run_until_parked();
    cx_0.update(|cx| { cx.background_executor().forbid_parking(); cx.quit(); });
    cx_0.run_until_parked();
    drop(cx_0);
    dispatcher.drain_tasks();
    drop(dispatcher);
}, on_failure);
```

Consequences you will hit:
- After your body returns, GPUI runs **another** `run_until_parked` with parking *forbidden*. A background task still waiting on something external panics there, not in your test body.
- `ref_counts_drop_handle()` means **leaked entities are detected at teardown**. Mutually-recursive `Entity` handles must use `WeakEntity`.

### 2.4 How the deterministic executor works

`TestScheduler` (`crates/scheduler/src/test_scheduler.rs`) owns:
- a `VecDeque` of runnables plus a `StdRng` seeded from the test seed; with `randomize_order` on, the next runnable is chosen pseudo-randomly, so **a different seed = a different legal interleaving**;
- a **fake clock** (`TestClock`), so `executor.now()`, `executor.timer(d)` and `advance_clock(d)` are virtual — no wall-clock waiting;
- an `allow_parking` flag, default **false**.

`cx.run_until_parked()` / `cx.executor().run_until_parked()` drains every runnable that can make progress and, when nothing is runnable, **jumps the clock to the next pending timer** and keeps going (`crates/gpui/src/executor.rs:213-222`). That is why it is needed after anything that spawns: nothing async happens until you yield.

If the scheduler runs out of work while something is still blocked, it panics:
```
Parking forbidden. Re-run with PENDING_TRACES=1 to show pending traces
```
(`crates/scheduler/src/test_scheduler.rs:475-486`). With `allow_parking()` the scheduler really blocks the thread, with a **hard 15-second timeout** that panics "Test timed out after 15 seconds while parking."

A second determinism guard, `assert_correct_thread`, panics with
"Detected activity on thread …, but test scheduler is running on … Your test is not deterministic."
if a non-scheduler thread touches GPUI state (unless `allow_parking` was used).

### 2.5 Environment variables

| var | read by | effect |
|---|---|---|
| `SEED` | `crates/gpui/src/test.rs:44,60,153`; `crates/scheduler/src/test_scheduler.rs:57` | forces the first/only seed; **overrides `seed`/`seeds` attributes** |
| `ITERATIONS` | `crates/gpui/src/test.rs:148`; `test_scheduler.rs:53` | overrides `iterations = N` |
| `OPERATIONS` | 24 randomized tests (`crates/text/src/tests.rs:53`, `crates/rope/src/rope.rs:1890`) | random operations per iteration (default 10) |
| `PENDING_TRACES=1` | `test_scheduler.rs:33,750` | print backtraces of pending tasks on "Parking forbidden" |
| `DEBUG_SCHEDULER` | `test_scheduler.rs:256,378` | verbose scheduler/clock tracing |
| `SCHEDULER_NONINTERACTIVE` | `test_scheduler.rs:61` | suppress per-seed `eprintln!` |
| `SIMPLE_TEXT` | `crates/util/src/util.rs:587` | `RandomCharIter` emits only a-z and newline (much easier failures) |
| `INITIAL_ENTRIES` | worktree random tests | initial fake-fs size |
| `ZED_LOG` / `RUST_LOG` | `crates/zlog/src/zlog.rs:34` | enable output from `zlog::init_test()` |
| `GPUI_RUN_UNTIL_PARKED_LOG=1` | `crates/gpui/src/executor.rs:229` | warn when `allow_parking` is enabled |
| `ZED_BENCH_HUGE` | `crates/benchmarks/benches/editor_render.rs` | adds the 100k-line benchmark input |
| `VISUAL_TEST_OUTPUT_DIR`, `UPDATE_BASELINES` | `crates/wu/src/wu/visual_tests.rs` | screenshot dir / baseline refresh |

On failure of a multi-seed run, `run_test` prints:
```
failing seed: 37
You can rerun from this seed by setting the environmental variable SEED to 37
```

### 2.6 `#[gpui::property_test]` (proptest-backed)

`crates/gpui_macros/src/property_test.rs`; docs at `crates/gpui_macros/src/gpui_macros.rs:208-268`.

- Same `&mut TestAppContext` / `BackgroundExecutor` handling; **`StdRng` is a hard compile error** ("breaks shrinking").
- All other args are forwarded to `proptest::property_test`; use `#[strategy = ...]` per-arg.
- `config = ProptestConfig { cases: 100, ..Default::default() }` is intercepted and wrapped in `gpui::apply_seed_to_proptest_config` so `$SEED` drives *both* the scheduler seed and proptest case generation. Passing `proptest_path` is a compile error.
- Real example: `crates/editor/src/editor_tests/property_test.rs:60`; `Arbitrary`/strategy helpers in `crates/sum_tree/src/property_test.rs`.
- Only 9 uses today. Prefer `#[gpui::test(iterations = N)]` + `StdRng` when the random choices depend on evolving state (the macro docs say exactly this).

---

## 3. Common test utilities and where they live

### 3.1 `FakeFs` — `crates/fs/src/fs.rs` (gated `#[cfg(feature = "test-support")]`, struct at :1398)

```rust
let fs = FakeFs::new(cx.executor());          // or cx.background_executor.clone()
fs.insert_tree(path!("/root"), json!({
    ".git": {},
    "dir": { "a.rs": "fn main() {}", "b.rs": "" },
    "empty_dir": null,
})).await;
```
- `insert_tree(path, serde_json::Value)` (`:1986`) — **object = dir, string = file contents, `null` = empty dir**; anything else panics. It is `#[must_use]` and returns a `BoxFuture`: **you must `.await` it**.
- There is **no `insert_tree!` macro** in this repo — it is a plain async method. `insert_tree_from_real_fs(dst, src)` (`:2025`) mirrors a real directory into the fake one.
- Other entry points: `fs.insert_file(path, Vec<u8>)` (`:1820`), `insert_symlink`, `touch_path`, `read_file_sync`, `set_next_mtime`, `set_case_sensitive`.
- Watcher-event control: `pause_events()`, `unpause_events_and_flush()`, `flush_events(count)`, `buffered_event_count()`, `clear_buffered_events()`, `simulate_watcher_overflow(root)` (`:1920-1965`), `create_file_before_next_watch_add(...)`.
- Git: `with_git_state_and_paths(...)`, `FakeGitRepository` in `crates/fs/src/fake_git_repo.rs`.
- **Latency simulation:** every `Fs` method calls `self.simulate_random_delay().await` (`:2668`, ~15 call sites), which is `BackgroundExecutor::simulate_random_delay` -> the deterministic dispatcher yields a seed-determined number of times. This is why FS-touching tests find real ordering bugs when you crank `iterations`.
- Reach it as `fs.as_fake()` when you hold an `Arc<dyn Fs>` (e.g. `app_state.fs.as_fake().insert_tree(...)`, `crates/editor/src/test/editor_lsp_test_context.rs:82`).
- `FakeFs` deliberately panics with "Failed to lock file system state, this execution would have caused a test hang" rather than deadlocking.

Real-filesystem counterpart: `util::test::TempTree::new(json!({...}))` (`crates/util/src/test.rs:11-31`) — writes a real tree in a `TempDir` and even runs `git init -b main` when it sees a `.git` key.

### 3.2 `util::test` — marked text (`crates/util/src/test/marked_text.rs`)

Marker syntax (doc comment at `:84-111`):
- `«…»` — a range/selection.
- `ˇ` (U+02C7 caron) — a cursor / point marker. Alone it means an empty range.
- Direction: put the caron **inside** a range, adjacent to a bound: `«ˇreversed»` vs `«forwardˇ»`.
- `•` anywhere in the input is replaced by a space (so trailing whitespace survives source formatting).

API:
- `marked_text_ranges(marked, ranges_are_directed: bool) -> (String, Vec<Range<usize>>)`. With `true`, a `«…»` **without** a caron panics: "missing 'ˇ' marker to indicate range direction".
- `marked_text_offsets(marked) -> (String, Vec<usize>)` — asserts every range is empty.
- `generate_marked_text(text, ranges, indicate_cursors) -> String` — the inverse; this is what assertion failures print.
- `marked_text_ranges_by` / `marked_text_offsets_by` + `TextRangeMarker::{Empty,Range,ReverseRange}` for **overlapping** ranges with custom marker chars (used by highlight tests).
- The self-test at `crates/util/src/test/marked_text.rs:260-281` is the best spec:
  `marked_text_ranges("one «ˇtwo» «threeˇ» «ˇfour» fiveˇ six", true)` -> ranges `7..4, 8..13, 18..14, 23..23`.

Also in `util::test`: `assert_set_eq!` / `set_eq!` (`crates/util/src/test/assertions.rs`).
Cross-platform literals live in `util_macros` (`crates/util_macros/src/util_macros.rs`): **`path!("/root/a.rs")`**, **`uri!("file:///x")`**, **`line_endings!("a\nb")`** — on Windows these become `C:\root\a.rs`, `file:///C:/x`, CRLF. Use them in *every* new test containing a path literal.
Random text: `util::RandomCharIter::new(&mut rng)` (`crates/util/src/util.rs:578-626`), `.with_simple_text()` or `SIMPLE_TEXT=1`.

### 3.3 Editor test contexts — `crates/editor/src/test/`

`EditorTestContext` (`crates/editor/src/test/editor_test_context.rs:36`) derefs to `gpui::VisualTestContext`.
`EditorLspTestContext` (`crates/editor/src/test/editor_lsp_test_context.rs:29`) derefs to `EditorTestContext` and adds `.lsp`, `.workspace`, `.buffer_lsp_url`.

Constructors:
- `EditorTestContext::new(cx).await` — FakeFs with `/root/{.git,file}`, a `Project::test`, a Plain Text buffer, a focused editor window, then `cx.run_until_parked()`.
- `EditorTestContext::new_multibuffer(cx, ["excerpt «one»", "excerpt two"])`
- `EditorTestContext::for_editor(window_handle, cx).await` / `for_editor_in(entity, visual_cx).await`
- `EditorLspTestContext::new(language, capabilities, cx).await`, plus `new_rust`, `new_typescript`, `new_tsx`, `new_html`, `new_markdown_with_rust`.

Key methods:

| method | notes |
|---|---|
| `cx.set_state("aˇbc")` | sets buffer text **and** selections from marked text; registers an assertion-context string printed on failure |
| `cx.set_selections_state("aˇbc")` | selections only; asserts text unchanged |
| `cx.assert_editor_state("aˇbc")` | asserts buffer text + selections (uses `pretty_assertions::assert_eq`) |
| `cx.assert_display_state(...)` | same but against *display* text (folds/inlays applied) |
| `cx.assert_state_with_diff(String)` | text + selections + expanded diff hunks (`+`/`-` prefixed lines) |
| `cx.assert_excerpts_with_selections(...)` | multibuffer form |
| `cx.assert_editor_background_highlights(key, ...)` / `assert_editor_text_highlights(key, ...)` | highlight ranges via marked text |
| `cx.update_editor(\|editor, window, cx\| ...)` / `cx.editor(...)` | editor access |
| `cx.update_buffer(...)`, `cx.buffer_text()`, `cx.display_text()`, `cx.buffer_snapshot()` | buffer access |
| `cx.set_head_text(base)`, `cx.set_index_text(...)`, `cx.clear_index_text()`, `cx.assert_index_text(...)` | git diff state |
| `cx.simulate_keystroke("cmd-k")` | **single** keystroke; does **not** run until parked (for timing tests) |
| `cx.simulate_keystrokes("c l o")` / `cx.simulate_input("hello")` | from `VisualTestContext`; **both call `run_until_parked` automatically** |
| `cx.run_until_parked()` | `self.cx.background_executor.run_until_parked()` |
| `cx.ranges(marked)`, `cx.display_point(marked)`, `cx.pixel_position(marked)` | marked text -> offsets/points/pixels |
| `cx.lsp_range(marked)`, `cx.to_lsp(offset)`, `cx.to_lsp_range(range)` | LSP coordinates (LSP ctx only) |
| `cx.language_registry()`, `cx.update_workspace(...)`, `cx.notify::<N>(params)` | |
| `EditorTestContext::root_path()` | `/root` on unix, `C:\root` on Windows |

Editor helpers outside the context, in `crates/editor/src/test.rs`: `marked_display_snapshot`, `select_ranges`, `assert_text_with_selections`, `editor_content_with_blocks(_and_width/_and_size)` (renders block decorations as `§ ...` lines), `set_block_content_for_tests`, `test_font()` (Helvetica; Courier New on Windows).

### 3.4 `VisualTestContext` and `TestAppContext`

`crates/gpui/src/app/visual_test_context.rs` and `crates/gpui/src/app/test_context.rs`.

Get a `VisualTestContext` with `VisualTestContext::from_window(*window_handle.deref(), cx)` or `cx.add_empty_window()`.
Methods: `simulate_keystrokes(&str)`, `simulate_input(&str)`, `dispatch_action(A)`, `dispatch_keystroke(Keystroke)`, `simulate_mouse_move/down/up/click`, `simulate_modifiers_change`, `simulate_capslock_change`, `simulate_resize(size)`, `simulate_close()`, `deactivate_window()`, `draw(origin, size, |window, cx| element)`, `debug_bounds("selector")`, `window_title()`, `run_until_parked()`, `update(|window, cx| ...)`, `focus(&entity)`.

`TestAppContext`: `update`/`read`/`read_global`/`set_global`/`update_global`, `add_window`, `add_empty_window`, `add_window_view`, `open_window`, `windows()`, `executor()` (returns a **`BackgroundExecutor`**), `foreground_executor()`, `to_async()`, `spawn`, `run_until_parked()`, `dispatch_action(window, A)`, `simulate_keystrokes(window, "...")`, `simulate_input(window, "...")`, `simulate_prompt_answer("Ok")`, `has_pending_prompt()`, `pending_prompt()`, `simulate_new_path_selection(...)`, `read_from_clipboard`/`write_to_clipboard`, `opened_url()`, `shown_system_notifications()`, `notifications::<T>()`, `events::<Evt, T>()`, `condition(entity, predicate).await`, `new_app()` (a second app on the same dispatcher), `quit()`.

`dispatch_action`, `simulate_keystrokes` and `simulate_input` all end with `run_until_parked()`.

### 3.5 Fake language servers — `crates/lsp/src/lsp.rs` + `crates/language/src/language_registry.rs`

Registration (`language_registry.rs:301`, gated `#[cfg(any(feature = "test-support", test))]`):

```rust
let mut fake_servers = language_registry.register_fake_lsp(
    "Rust",
    FakeLspAdapter { capabilities, ..Default::default() },
);
language_registry.add(Arc::new(language));
// ...open a buffer of that language, then:
let fake_server = fake_servers.next().await.unwrap();   // UnboundedReceiver<FakeLanguageServer>
```
Also available: `register_fake_lsp_adapter` (adapter only), `register_fake_lsp_server` (server only), `register_fake_available_lsp_adapter`. `FakeLspAdapter` has an `initializer: Option<Box<dyn Fn(&mut FakeLanguageServer)>>` hook.

`FakeLanguageServer` API (`crates/lsp/src/lsp.rs:1961-2090`):
- `set_request_handler::<request::Formatting, _, _>(|params, cx| async move { Ok(...) })` -> returns an `UnboundedReceiver<()>` that fires once per handled request. It **replaces** any existing handler for that type, and it awaits `executor.simulate_random_delay()` before responding.
- `handle_notification::<N, _>(|params, cx| ...)`
- `remove_request_handler::<T>()`
- `notify::<notification::PublishDiagnostics>(params)`
- `receive_notification::<N>().await` / `try_receive_notification::<N>().await` (skips other methods)
- `request::<T>(params, timeout).await`
- `start_progress(token).await`, `start_progress_with(token, begin, timeout).await`, `end_progress(token)`

`EditorLspTestContext::set_request_handler::<T,_,_>(|uri, params, cx| ...)` (`editor_lsp_test_context.rs:483`) is the same thing with the buffer's URI pre-bound.

### 3.6 `Project::test`, `AppState::test`, `SettingsStore::test`

- `Project::test(fs, root_paths, cx).await` — `crates/project/src/project.rs:1532` (`#[cfg(feature = "test-support")]`). Builds a `LanguageRegistry::test`, `FakeHttpClient::with_404_response()`, a `Client`, a `UserStore`, then `find_or_create_worktree` for each path. Variant: `Project::test_with_worktree_trust(...)`.
- `AppState::test(cx) -> Arc<AppState>` — `crates/workspace/src/workspace.rs:1149`. Creates a `SettingsStore::test` if absent, a `FakeFs` set as the global `dyn Fs`, `LanguageRegistry::test`, `FakeHttpClient`, `Session::test`, and calls `theme_settings::init(LoadThemes::JustBase, cx)` for you. Reach the fake fs via `app_state.fs.as_fake()`.
- `SettingsStore::test(cx)` — `crates/settings/src/settings_store.rs:514`; parses `crate::test_settings()` once (cached). Mutate with `store.update_user_settings(cx, |settings| ...)` (`:527`).
- `MultiWorkspace::test_new(project, window, cx)` for a workspace window.
- `theme_settings::init(theme::LoadThemes::JustBase, cx)` is the standard call (75 uses); it wraps `theme::init` (7 direct uses) and adds settings observation. **Use `theme_settings::init`** unless you specifically do not want settings wiring.
- `assets::Assets.load_test_fonts(cx)` — required whenever text is measured or rendered.

### 3.7 THE standard test-init boilerplate (quoted verbatim)

**(a) Minimal, non-UI (project / worktree / git-store style)** — `C:/Users/USER/Documents/wu-main/crates/project/tests/integration/project_tests.rs:16611`:

```rust
pub fn init_test(cx: &mut gpui::TestAppContext) {
    zlog::init_test();

    cx.update(|cx| {
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        release_channel::init(semver::Version::new(0, 0, 0), cx);
    });
}
```

**(b) Editor tests — the single most copied block in the repo** — `C:/Users/USER/Documents/wu-main/crates/editor/src/editor_tests.rs:37045`:

```rust
pub(crate) fn init_test(cx: &mut TestAppContext, f: fn(&mut AllLanguageSettingsContent)) {
    cx.update(|cx| {
        assets::Assets.load_test_fonts(cx);
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        release_channel::init(semver::Version::new(0, 0, 0), cx);
        crate::init(cx);
    });
    zlog::init_test();
    update_test_language_settings(cx, &f);
}
```

**(c) Full workspace/panel UI tests** — `C:/Users/USER/Documents/wu-main/crates/project_panel/src/project_panel_tests.rs:11431`:

```rust
fn init_test_with_editor(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let app_state = AppState::test(cx);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        editor::init(cx);
        crate::init(cx);
        workspace::init(app_state, cx);

        cx.update_global::<SettingsStore, _>(|store, cx| {
            store.update_user_settings(cx, |settings| {
                settings
                    .project_panel
                    .get_or_insert_default()
                    .auto_fold_dirs = Some(false);
                settings.project.worktree.file_scan_exclusions =
                    Some(SplicingVec::from(Vec::new()));
                settings.project.worktree.file_scan_depth = Some(0);
            });
        });
    });
}
```

**(d) Workspace-only** — `C:/Users/USER/Documents/wu-main/crates/workspace/src/workspace.rs:16118`:

```rust
pub fn init_test(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        cx.set_global(db::AppDatabase::test_new());
        theme_settings::init(theme::LoadThemes::JustBase, cx);
    });
}
```

**(e) Debugger / multi-panel** — `C:/Users/USER/Documents/wu-main/crates/debugger_ui/src/tests.rs:36` adds `terminal_view::init`, `command_palette_hooks::init`, `dap_adapters::init`, plus an async `init_test_workspace(&project, cx).await` that loads the debug and terminal panels.

Ordering that matters: `SettingsStore` **first** (everything else reads it), then `theme_settings::init`, then per-crate `init(cx)`, then `workspace::init(app_state, cx)` last.

### 3.8 `test-support` feature flags — the rule

Two gates, used ~342 times across the repo:
- `#[cfg(any(test, feature = "test-support"))]` — helper that must be visible to **other crates'** tests.
- `#[cfg(test)]` — the test module itself.

`Cargo.toml` pattern (`crates/project/Cargo.toml:21-33`, `crates/editor/Cargo.toml:16-24`):

```toml
[features]
test-support = [
    "gpui/test-support",
    "language/test-support",
    "settings/test-support",
    # ...every dependency whose fakes you re-export
]

[dev-dependencies]
gpui = { workspace = true, features = ["test-support"] }
language = { workspace = true, features = ["test-support"] }
project = { workspace = true, features = ["test-support"] }   # self, so integration tests see own fakes
pretty_assertions.workspace = true
unindent.workspace = true
```

**Rules:**
1. To use another crate's fakes (`FakeFs`, `FakeLanguageServer`, `Project::test`, ...), add that crate to your `[dev-dependencies]` with `features = ["test-support"]`. Never rely on incidental feature unification.
2. If your own crate exports fakes its own integration tests need, add a **self** dev-dependency with `features = ["test-support"]` (see `fs`, `project`, `worktree`).
3. If crate A's `test-support` needs fakes from B, add `"B/test-support"` to A's `test-support` list.
4. `crates/util` is an exception: `pub mod test;` is unconditional (`crates/util/src/util.rs:25`), but its `test-support` feature gates `rand` + `util_macros`, so `path!` / `RandomCharIter` still need it.

---

## 4. Snapshot / golden tests and randomized ("random") tests

### 4.1 Snapshot tests

**There is no `insta`, `expect-test`, or goldenfile crate in this repo.** "Golden" assertions are in-source string comparisons via `indoc!` / `unindent()` + `pretty_assertions::assert_eq!`, or via the marked-text helpers. `editor_content_with_blocks(...)` (`crates/editor/src/test.rs:173`) renders an editor to a multiline `String` with `§`-prefixed block lines and you assert against an `indoc!` literal — the closest thing to a golden test.

The only image-based golden tests are the **visual tests**: `crates/wu/src/wu/visual_tests.rs` (+ `crates/wu/src/visual_test_runner.rs`). They are `#[ignore]`d, macOS-only, need Screen Recording permission and the main thread. Header docs (`:17-33`):
```bash
cargo test -p wu visual_tests -- --ignored --test-threads=1
UPDATE_BASELINES=1 cargo test -p wu visual_tests -- --ignored --test-threads=1
```
Output dir: `$VISUAL_TEST_OUTPUT_DIR` or `target/visual_tests`. Do not touch these in a normal agent session.

### 4.2 Randomized / fuzz tests — the strong Zed tradition

Roughly 35 `test_random_*` tests. Canonical shape (`C:/Users/USER/Documents/wu-main/crates/text/src/tests.rs:51-56`, `crates/rope/src/rope.rs:1887-1892`):

```rust
#[gpui::test(iterations = 100)]
fn test_random_edits(mut rng: StdRng) {
    let operations = env::var("OPERATIONS")
        .map(|i| i.parse().expect("invalid `OPERATIONS` variable"))
        .unwrap_or(10);
    // ...
    buffer.check_invariants();
}
```

Where they live: `crates/text/src/tests.rs` (`test_random_edits`, `test_random_concurrent_edits`), `crates/rope/src/rope.rs` (`test_random_rope`), `crates/rope/src/chunk.rs`, `crates/text/src/patch.rs`, `crates/sum_tree/src/sum_tree.rs`, `crates/language/src/buffer_tests.rs` (`test_random_collaboration`), `crates/language/src/syntax_map/syntax_map_tests.rs`, `crates/multi_buffer/src/multi_buffer_tests.rs`, `crates/editor/src/display_map/{block_map,fold_map,inlay_map,tab_map,wrap_map}.rs`, `crates/editor/src/split.rs`, `crates/diagnostics/src/diagnostics_tests.rs`, `crates/worktree/tests/integration/worktree_tests.rs` (`test_random_worktree_changes`, `test_random_git_updates_with_watcher_overflows`), `crates/gpui/examples/testing.rs`.

Building blocks:
- `util::RandomCharIter::new(&mut rng).take(n).collect::<String>()`
- `buffer.randomly_edit(&mut rng, 5)`, `buffer.randomly_undo_redo(&mut rng)`, `buffer.random_byte_range(0, &mut rng)`, `buffer.check_invariants()` (test-support methods on `text::Buffer`)
- `fs.simulate_watcher_overflow(root)` for FS fuzzing
- a plain `String`/`Vec` "reference model" asserted against after every operation.

Running one:
```bash
cargo test -p text test_random_edits                       # 100 iterations, seeds 0..100
SEED=37 cargo test -p text test_random_edits -- --nocapture
SEED=37 OPERATIONS=200 ITERATIONS=1 cargo test -p text test_random_edits -- --nocapture
SIMPLE_TEXT=1 SEED=37 cargo test -p rope test_random_rope  # readable a-z text
ITERATIONS=10000 cargo test -p multi_buffer test_random_multibuffer   # soak
```
When a seed reproduces, pin it in-source: `#[gpui::test(iterations = 20, seeds(31))]` (this exact form is used in-tree, as is `seeds(340, 472)`).

**Write a random test when** you touch a data structure with an invariant that hand-written cases can't cover: rope / sum-tree / patch / CRDT edits, incremental maps (fold, inlay, tab, wrap, block), multibuffer excerpt bookkeeping, worktree scanning under concurrent FS events, or anything where operations interleave. Always pair it with a reference model plus `check_invariants()`.

---

## 5. Benchmarks and performance measurement

### 5.1 Criterion benches (`harness = false`)

| crate | bench target | file |
|---|---|---|
| `benchmarks` | `editor_render`, `display_map`, `markdown_renderer` | `crates/benchmarks/benches/*.rs` |
| `extension_host` | `extension_compilation_benchmark` | |
| `fuzzy_nucleo` | `match_benchmark` | |
| `gpui_wgpu` | `layout_line` | |
| `language` | `highlight_map` | |
| `project_panel` | `sorting` | |
| `rope` | `rope_benchmark` | |

Criterion 0.5 with `html_reports` (`Cargo.toml:427`). **No divan.**

`crates/benchmarks` is the GPUI-aware one. It uses `#[gpui::bench]` (`crates/gpui_macros/src/bench.rs`), whose **only** accepted arguments are:
`fps = N`, `inputs = EXPR`, `input_name = "..."`, `group = "..."`, `sample_size = N`
(`input_name` / `group` / `sample_size` require `inputs`; async bench fns are rejected). It generates a `fn(&mut criterion::Criterion)`, gives the body a `&mut BenchAppContext` with `cx.bench_iter(|cx| ...)`, and prints a `BenchReport` including **frame-budget overruns** at the configured fps. Wire up with `gpui::bench_group!(benches, a, b, c);` and `gpui::bench_main!(benches);` (`crates/gpui/src/gpui.rs:122-137`). Requires `gpui` / `gpui_platform` with the `bench-support` feature in dev-dependencies.

Run:
```bash
cargo bench -p benchmarks                                   # all three
cargo bench -p benchmarks --bench editor_render
cargo bench -p benchmarks --bench editor_render -- multi_cursor   # criterion filter
ZED_BENCH_HUGE=1 cargo bench -p benchmarks --bench editor_render  # adds the 100k-line input
cargo bench -p rope --bench rope_benchmark
```

### 5.2 Standalone benchmark binaries

These are `[[bin]]` crates, not criterion; run with `cargo run --release -p ...`:

- `crates/editor_benchmarks` — opens a file in a real editor and runs search/replace:
  `cargo run --release -p editor_benchmarks -- FILE QUERY [--regex] [--whole-word] [--case-sensitive] [--single] [-r REPLACE]`
- `crates/worktree_benchmarks` — background worktree scan timing over a real dir:
  `cargo run --release -p worktree_benchmarks -- PATH_TO_WORKTREE_ROOT`
- `crates/fs_benchmarks` — real-fs op timing.
- `crates/project_benchmarks` — clap-based; can drive a **remote** project (depends on `remote` with `build-remote-server-binary`).

### 5.3 `tooling/perf` — the `#[perf]` test profiler

`tooling/perf/src/main.rs` (docs at the top) + the `#[perf]` attribute in `crates/util_macros/src/util_macros.rs:183`.
Needs **`hyperfine` on PATH**. Cargo aliases live in `.cargo/config.toml:7-10`:

```bash
cargo perf-test -p gpui                 # rebuilds with --cfg perf_enabled, profile release-fast
cargo perf-test --workspace             # VERY slow
cargo perf-test -p editor -- --important --quiet
cargo perf-test -p gpui -- --json=before        # writes .perf-runs/before.gpui.json
cargo perf-compare after before                 # markdown diff
cargo perf-compare --save=out.md after before
```
`#[perf]` arguments: an importance level (`critical` / `important` / `average` / `iffy` / `fluff`), `weight = N` (default 50), `iterations = N`. It implies `#[test]` and composes with `#[gpui::test]` (put `#[perf(iterations = 1, critical)]` **above** `#[gpui::test]`). Do not apply it to disk-IO-heavy tests. 84 uses today, concentrated in `crates/search/src/buffer_search.rs` and `crates/gpui/src/style.rs`.

### 5.4 Scripts

- `script/cargo` — a **Node** wrapper around cargo that injects `--timings` for `build/check/run/test` and post-processes `target/cargo-timings/*.html` (upload to Snowflake for Zed staff). **It is a pure passthrough unless `git remote -v` mentions `zed-industries/zed`, which is false in this repo**, so in Anna it is functionally just `cargo`. `./script/cargo --init` installs a shell alias (bash/zsh/fish/PowerShell). Not worth using here.
- `script/clippy` — `cargo clippy --workspace --release --all-targets --all-features -- --deny warnings`, plus `cargo shear`, `typos`, `buf lint/format` when locally installed. **`.rules` says: use `./script/clippy`, not `cargo clippy`.** Windows variant: `script/clippy.ps1`.
- `script/histogram` — Python (pandas/matplotlib/seaborn). Parses `measurement: 12ms` lines out of log files and plots per-measurement histograms: `python script/histogram log1.txt log2.txt`.
- `script/memory-benchmark` — **macOS only** (uses `footprint(1)`). Alternates launching Anna and Zed with throwaway `--user-data-dir`s and reports the median footprint. `RUNS=5 SETTLE_SECONDS=30 script/memory-benchmark`.
- `cargo xtask ...` (alias -> `tooling/xtask`): `clippy`, `licenses`, `package-conformity`, `publish-gpui`, `wsl-sandbox-tests`, `setup-webrtc`, `web-examples`.

**When should an agent benchmark?** Only when the change is explicitly about performance (rendering, rope/sum-tree, worktree scan, search) or a reviewer asks. Benchmarks need release builds of large crates and take minutes to hours. Default to tests.

---

## 6. Exact commands

```bash
# --- one test ---------------------------------------------------------------
cargo test -p editor test_concurrent_format_requests
cargo test -p editor test_concurrent_format_requests -- --exact --nocapture
cargo test -p text test_random_edits -- --nocapture

# --- one crate --------------------------------------------------------------
cargo test -p editor
cargo test -p gpui --features test-support        # gpui's own tests need the feature
cargo test -p gpui --example testing --features test-support

# --- crates whose lib has `test = false` (fs, project, worktree) -------------
cargo test -p project --test integration
cargo test -p project --test integration test_default_session_work_dirs
cargo test -p worktree --test integration test_random_worktree_changes
cargo test -p fs --test integration

# --- whole suite ------------------------------------------------------------
cargo test --workspace                            # NOTE: bare `cargo test` only tests crates/wu
cargo nextest run --workspace                     # honours .config/nextest.toml
cargo nextest run -p editor -E 'test(test_random_split_editor)'

# --- ignored / special ------------------------------------------------------
cargo test -p wu visual_tests -- --ignored --test-threads=1       # macOS only
cargo test -p project debug_parse_tokens -- --nocapture --ignored

# --- lint (per .rules, instead of cargo clippy) -----------------------------
./script/clippy
./script/clippy -p editor
powershell -File script/clippy.ps1                # Windows

# --- benches / perf ---------------------------------------------------------
cargo bench -p benchmarks
cargo perf-test -p gpui
cargo perf-compare new old
```

### Gotchas in these commands

- **`Cargo.toml:185` sets `default-members = ["crates/wu"]`.** A bare `cargo test` / `cargo build` only touches `crates/wu`. Always pass `-p <crate>` or `--workspace`.
- `crates/{fs,project,worktree}` have `[lib] test = false` and a single `[[test]] name = "integration"` with `required-features = ["test-support"]`. `cargo test -p project` works because the crate dev-depends on itself with that feature; `--test integration` is the explicit form.
- 81 crates set `doctest = false`, so doc examples are mostly not compiled. `crates/gpui_macros` doc examples **are** compiled — keep them valid if you edit the macros.
- `.cargo/config.toml` pins `jobs = 4` and `-C symbol-mangling-version=v0`; on Windows it also adds `--cfg windows_slim_errors` and `-C target-feature=+crt-static`.

### nextest configuration — `C:/Users/USER/Documents/wu-main/.config/nextest.toml`

- Default `slow-timeout = { period = "60s", terminate-after = 1 }` — **a test running >60s is killed**.
- `package(db)` runs in a `sequential-db-tests` group with `max-threads = 1`.
- Priority boosts so the slowest start first: `worktree::test_random_worktree_changes` (100), `extension_host::test_extension_store_with_test_extension` (99).
- 300s timeout overrides for: `test_rainbow_bracket_highlights`, `test_wrapped_invisibles_drawing`, `test_basic_following`, `test_random_diagnostics_blocks`, `extension_host::test_extension_store_with_test_extension`, `language_model::test_from_image_downscales_to_default_5mb_limit`, a list of `vim` tests, `editor::test_random_split_editor`, `editor::test_random_blocks`.
- Some filters reference Zed packages that no longer exist here (`collab`, `vim`, `language_model`) — harmless leftovers.

**If your new test legitimately takes >60s under nextest, add an override here rather than shrinking coverage.**

### Windows-specific

- **No CI workflow runs tests in this repo** (`.github/workflows/` has only `release.yml`), so you are the CI. Run at least the affected crate locally.
- `#[cfg(unix)]` / `#[cfg(not(windows))]`-only tests exist in: `crates/fs/tests/integration/fs_tests.rs:594,614,798`, `crates/worktree/tests/integration/worktree_tests.rs:1170,1241`, `crates/project/tests/integration/project_tests.rs:71,192,13566,17474` (symlink behaviour — that file carries an explicit note that POSIX symlinks on Windows are opt-in), `crates/open_path_prompt/src/open_path_prompt_tests.rs:51,428`, `crates/editor/src/editor_tests.rs:18963`.
- Windows-only branches: `crates/editor/src/test.rs:35` (test font Courier New instead of Helvetica), `EditorTestContext::root_path()` / `EditorLspTestContext::root_path()` -> `C:\root`, `crates/gpui/src/platform/test/window.rs:438`, `crates/project_panel/src/tests/undo.rs:32`, `crates/project/tests/integration/lsp_store.rs:331`.
- **Use `path!("/root/x")`, `uri!("file:///x")`, `line_endings!("a\nb")` in every new test with a literal.** A bare `"/root/x"` will pass on Linux/macOS and fail here.
- `crates/wu/src/wu.rs:3134` is `#[ignore = "This test has timing issues across platforms."]` — the repo's own admission that wall-clock-dependent tests are flaky cross-platform.
- `script/memory-benchmark` and the visual tests are macOS-only; `script/clippy` is bash — use `script/clippy.ps1` on Windows.

### All 15 `#[ignore]`s

`crates/editor/src/split.rs:4139`; `crates/fs/tests/integration/fs_tests.rs:1044` ("stress test; run explicitly when needed"); `crates/gpui_platform/src/gpui_platform.rs:114,143,174` ("Requires macOS main thread"); `crates/project/src/lsp_store/semantic_tokens.rs:880` (debug helper, run with `--ignored --nocapture`); `crates/project/tests/integration/project_tests.rs:14211,15612,15771`; `crates/settings_content/src/terminal.rs:553`; `crates/wu/src/wu/visual_tests.rs:426,438,479`; `crates/wu/src/wu.rs:3134`.

---

## 7. Flakiness rules

1. **Never `smol::Timer::after(...)` in a GPUI test.** Root `.rules`, "Timers in tests":
   > Use `cx.background_executor().timer(duration).await` (or `cx.background_executor.timer(duration).await` in `TestAppContext`) so the work is scheduled on GPUI's dispatcher. Avoid `smol::Timer::after(...)` for test timeouts when you rely on `run_until_parked()`, because it may not be tracked by GPUI's scheduler and can lead to "nothing left to run" when pumping.

   Verified: there are **zero** `smol::Timer::after` uses in `crates/` today. Keep it that way.

2. **Yield explicitly; never "wait a bit".** After anything that spawns (`cx.spawn`, `cx.background_spawn`, `.detach()`, an LSP request, a FakeFs write), call `cx.run_until_parked()` or `cx.executor().run_until_parked()`. Do not sleep.

3. **Use the fake clock for debounces/timeouts**, then pump:
   ```rust
   cx.executor().advance_clock(Duration::from_millis(200));
   cx.run_until_parked();
   ```
   (430 `advance_clock` uses; e.g. `crates/project_panel/src/project_panel_tests.rs:11399,11851`, `crates/editor/src/editor_tests.rs:16071` with `super::FORMAT_TIMEOUT`.) `advance_clock` makes timers ready but **runs nothing** — the following `run_until_parked` is mandatory.

4. **`.await` `Task`s directly when you can.** `let x = entity.update(cx, |e, cx| e.do_thing(cx)).await;` is more deterministic than pump-and-poll. A dropped `Task` is a *cancelled* task — store it, `detach()` it, or await it.

5. **`allow_parking()` is a last resort.** Only for genuine external I/O (a real OS thread, real filesystem, a real process). Examples: `crates/project/tests/integration/project_tests.rs:99,116` (`test_block_via_channel`, `test_block_via_smol`); `crates/gpui/examples/testing.rs` `test_allow_parking`. It disables the determinism thread check and imposes a real 15-second wall-clock timeout. `forbid_parking()` turns it back off. Prefer `FakeFs` + fake timers.

6. **Raise `iterations`, do not add sleeps.** If a test fails only occasionally it is an ordering bug. Reproduce with `ITERATIONS=1000`, take the failing seed from the harness output, pin `seeds(N)`, fix the bug. `retries = N` exists (4 uses) but is a bandage.

7. **Never assert on wall-clock durations** (`Instant::now()` deltas). Use `cx.executor().now()`, which reads the fake clock.

8. **Do not cross threads.** `std::thread::spawn` touching GPUI state trips "Detected activity on thread ... Your test is not deterministic." Use `cx.background_spawn`.

9. **Diagnosing a hang:** `PENDING_TRACES=1 cargo test -p X my_test` prints backtraces of the stuck wakers on the "Parking forbidden" panic. `DEBUG_SCHEDULER=1` traces the clock and queue. `GPUI_RUN_UNTIL_PARKED_LOG=1` warns when parking got enabled.

10. **Keep tests under 60 s** (nextest kills them) or add an override in `.config/nextest.toml`.

---

## 8. Footguns

- **`cargo test` alone tests only `crates/wu`** (`default-members`). Use `-p` or `--workspace`.
- **`cargo test -p project` will not run any `src/` test** — the lib is `test = false`; everything lives in `tests/integration/`.
- **`fs.insert_tree(...)` is `#[must_use]` and async.** Forgetting `.await` silently creates nothing.
- **`insert_tree` JSON grammar is strict**: object = dir, string = file, `null` = empty dir. Numbers/arrays panic with "JSON object must contain only objects, strings, or null".
- **Path literals**: a bare `"/root"` breaks on Windows. Use `path!`, `uri!`, `line_endings!`.
- **`marked_text_ranges(_, true)` panics** on a `«...»` with no `ˇ` ("missing 'ˇ' marker to indicate range direction"). `assert_editor_state` uses `true`; `cx.ranges()` uses `false`.
- **The caron is `ˇ` (U+02C7)**, not `^` and not `|`. Range brackets are `«` `»` (U+00AB/U+00BB). Copy them from an existing test rather than typing them.
- **`•` in marked text becomes a space** — surprising if your fixture legitimately contains a bullet.
- **`cx.simulate_keystroke` (singular) does NOT pump**; `cx.simulate_keystrokes`, `cx.simulate_input` and `cx.dispatch_action` DO. Mixing them up is the number-one cause of "the action didn't do anything".
- **`TestAppContext` has no `.read(cx)` on entities** — use `entity.read_with(cx, |e, cx| ...)`. (Called out explicitly in `crates/gpui/examples/testing.rs`.)
- **Nested `entity.update(...)` panics.** Always use the inner `cx` inside an update closure (root `.rules`, GPUI section).
- **Entity leaks fail at teardown, not in your test.** The macro installs `ref_counts_drop_handle()`. Break `Entity` cycles with `WeakEntity`.
- **`$SEED` overrides `seed`/`seeds` attributes entirely** — a pinned regression seed is skipped if you have `SEED` exported in your shell.
- **`set_request_handler::<T>` replaces** the previous handler for `T`, and the returned `UnboundedReceiver<()>` is your only signal that the request was actually served (`rx.next().await`).
- **`FakeLanguageServer` arrives via a stream**: `fake_servers.next().await.unwrap()`, and only after a buffer of that language is opened. A missing `cx.run_until_parked()` before it looks like a hang.
- **`SettingsStore` must be the first global you set**; `theme_settings::init` and every `crate::init(cx)` read it.
- **`theme::init` vs `theme_settings::init`**: use `theme_settings::init(theme::LoadThemes::JustBase, cx)` (75 uses). `theme::init` alone skips settings wiring and font-size observation.
- **`assets::Assets.load_test_fonts(cx)` is required** for anything that measures or renders text; without it layout assertions are meaningless or panic.
- **`AppState::test(cx)` already calls `theme_settings::init`** — calling it again is harmless but redundant (project_panel does exactly that).
- **`test = false` + `required-features`** means `cargo test --workspace` without `--all-features` still builds those integration targets *only* because of the self dev-dependency. If you create a new crate with the same layout, copy that dev-dependency line or your tests will silently never run.
- **`#[perf]` implies `#[test]`** — do not add both, and do not put `#[perf]` on IO-heavy tests (file locks are not released fast enough across iterations).
- **`README.md` HARD RULE**: root `.rules` requires prepending a `> [!IMPORTANT]` / "Remove this line..." pair to `README.md` when modifying source files. Repo policy, not a test rule, but it will come up in any PR.
- No `.rules` files exist inside individual crates today (only the root one), despite the root file saying crate-specific rules belong there.

---

## 9. Copy-paste test templates

### Template 1 — Pure logic test (no GPUI)

Shape from `C:/Users/USER/Documents/wu-main/crates/util/src/util.rs:794-828`:

```rust
// ...at the end of crates/<crate>/src/<module>.rs...

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extend_sorted() {
        let mut vec = vec![];

        extend_sorted(&mut vec, vec![21, 17, 13, 8, 1, 0], 5, |a, b| b.cmp(a));
        assert_eq!(vec, &[21, 17, 13, 8, 1]);

        extend_sorted(&mut vec, vec![101, 19, 17, 8, 2], 8, |a, b| b.cmp(a));
        assert_eq!(vec, &[101, 21, 19, 17, 13, 8, 2, 1]);
    }
}
```

Randomized variant (`crates/rope/src/rope.rs:1887`, `crates/text/src/tests.rs:51`):

```rust
#[gpui::test(iterations = 100)]
fn test_random_rope(mut rng: StdRng) {
    let operations = env::var("OPERATIONS")
        .map(|i| i.parse().expect("invalid `OPERATIONS` variable"))
        .unwrap_or(10);

    let mut expected = String::new();   // reference model
    let mut actual = Rope::new();
    for _ in 0..operations {
        let len = rng.random_range(0..=64);
        let new_text: String = RandomCharIter::new(&mut rng).take(len).collect();
        // ...apply the same edit to both...
        assert_eq!(actual.text(), expected);
    }
}
```

### Template 2 — GPUI async entity test (FakeFs + Project + run_until_parked)

Assembled from `C:/Users/USER/Documents/wu-main/crates/project/tests/integration/project_tests.rs:132-171` and `:16611`:

```rust
use fs::FakeFs;
use gpui::TestAppContext;
use project::Project;
use serde_json::json;
use settings::SettingsStore;
use std::path::Path;
use util::path;

pub fn init_test(cx: &mut gpui::TestAppContext) {
    zlog::init_test();

    cx.update(|cx| {
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        release_channel::init(semver::Version::new(0, 0, 0), cx);
    });
}

#[gpui::test]
async fn test_default_session_work_dirs(cx: &mut gpui::TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/root"),
        json!({
            "dir-project": { "src": { "main.rs": "fn main() {}" } },
            "single-file.rs": "fn helper() {}"
        }),
    )
    .await;

    let project = Project::test(
        fs.clone(),
        [Path::new(path!("/root/dir-project"))],
        cx,
    )
    .await;

    // Anything spawned needs an explicit yield before you can observe it.
    cx.run_until_parked();

    let work_dirs = project.read_with(cx, |project, cx| project.default_path_list(cx));
    let ordered_paths = work_dirs.ordered_paths().cloned().collect::<Vec<_>>();
    assert_eq!(ordered_paths, vec![std::path::PathBuf::from(path!("/root/dir-project"))]);
}
```

Multi-client variant (two contexts share one dispatcher — `crates/remote_server/src/remote_editing_tests.rs:57`, `crates/gpui/examples/testing.rs`):

```rust
#[gpui::test]
async fn test_basic_remote_editing(cx: &mut TestAppContext, server_cx: &mut TestAppContext) {
    let fs = FakeFs::new(server_cx.executor());
    // ...
}

#[gpui::test(iterations = 10)]
fn test_random_interleaving(cx_a: &mut TestAppContext, cx_b: &mut TestAppContext, mut rng: StdRng) {
    // ...
    cx_a.run_until_parked();   // pumps BOTH apps, in a seed-determined order
}
```

### Template 3 — Editor + fake LSP test

Verbatim from `C:/Users/USER/Documents/wu-main/crates/editor/src/editor_tests.rs:37045` (`init_test`) and `:19032-19085` (`test_concurrent_format_requests`):

```rust
pub(crate) fn init_test(cx: &mut TestAppContext, f: fn(&mut AllLanguageSettingsContent)) {
    cx.update(|cx| {
        assets::Assets.load_test_fonts(cx);
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        release_channel::init(semver::Version::new(0, 0, 0), cx);
        crate::init(cx);
    });
    zlog::init_test();
    update_test_language_settings(cx, &f);
}

#[gpui::test]
async fn test_concurrent_format_requests(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            document_formatting_provider: Some(lsp::OneOf::Left(true)),
            ..Default::default()
        },
        cx,
    )
    .await;

    cx.set_state(indoc! {"
        one.twoˇ
    "});

    // The format request takes a long time. When it completes, it inserts
    // a newline and an indent before the `.`
    cx.lsp
        .set_request_handler::<lsp::request::Formatting, _, _>(move |_, cx| {
            let executor = cx.background_executor().clone();
            async move {
                executor.timer(Duration::from_millis(100)).await;   // GPUI timer, NOT smol::Timer
                Ok(Some(vec![lsp::TextEdit {
                    range: lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(0, 3)),
                    new_text: "\n    ".into(),
                }]))
            }
        });

    // Submit a format request.
    let format_1 = cx
        .update_editor(|editor, window, cx| editor.format(&Format, window, cx))
        .unwrap();
    cx.executor().run_until_parked();

    // Submit a second format request.
    let format_2 = cx
        .update_editor(|editor, window, cx| editor.format(&Format, window, cx))
        .unwrap();
    cx.executor().run_until_parked();

    // Wait for both format requests to complete
    cx.executor().advance_clock(Duration::from_millis(200));   // fake clock
    format_1.await.unwrap();
    format_2.await.unwrap();

    // The formatting edits only happens once.
    cx.assert_editor_state(indoc! {"
        one
            .twoˇ
    "});
}
```

### Template 4 (bonus) — Editor-only test, no LSP

From `C:/Users/USER/Documents/wu-main/crates/editor/src/editor_tests.rs:1712-1735`:

```rust
#[gpui::test]
async fn test_fold_with_unindented_multiline_raw_string(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    cx.update_buffer(|buffer, cx| buffer.set_language(Some(rust_lang()), cx));
    cx.set_state(indoc! {"
        ˇfn main() {
            let s = 1;
        }
    "});

    cx.update_editor(|editor, window, cx| {
        editor.fold_at_level(&FoldAtLevel(1), window, cx);
        assert_eq!(
            editor.display_text(cx),
            indoc! {"
                fn main() {⋯
                }
            "},
        );
    });
}
```

### Template 5 (bonus) — Full workspace / panel UI test

Boilerplate from `C:/Users/USER/Documents/wu-main/crates/project_panel/src/project_panel_tests.rs:11431` plus the window setup at `crates/workspace/src/workspace.rs:16127`:

```rust
#[gpui::test]
async fn test_panel_behaviour(cx: &mut TestAppContext) {
    init_test_with_editor(cx);   // see section 3.7(c): AppState::test + editor::init + workspace::init

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/root"), json!({ "file.rs": "fn main() {}\n" })).await;

    let project = Project::test(fs.clone(), [path!("/root").as_ref()], cx).await;
    let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
    let workspace = window.root(cx).unwrap();
    let cx = &mut gpui::VisualTestContext::from_window(*window.deref(), cx);

    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-shift-p");                      // auto-pumps
    cx.dispatch_action(SomeAction);                             // auto-pumps
    cx.executor().advance_clock(Duration::from_millis(200));    // for debounces
    cx.run_until_parked();

    workspace.read_with(cx, |workspace, cx| { /* assertions */ });
}
```

---

## 10. Quick file index

| what | where |
|---|---|
| repo-wide rules (incl. timers-in-tests) | `C:/Users/USER/Documents/wu-main/.rules` |
| `#[gpui::test]` macro | `crates/gpui_macros/src/test.rs` |
| `#[gpui::property_test]` macro | `crates/gpui_macros/src/property_test.rs` |
| `#[gpui::bench]` macro | `crates/gpui_macros/src/bench.rs` |
| macro doc comments (attribute reference) | `crates/gpui_macros/src/gpui_macros.rs:151-268` |
| `run_test`, seed math, `Observation` | `crates/gpui/src/test.rs` |
| `TestAppContext`, `VisualTestContext` | `crates/gpui/src/app/test_context.rs`, `crates/gpui/src/app/visual_test_context.rs` |
| deterministic scheduler / parking panics | `crates/scheduler/src/test_scheduler.rs` |
| executor test helpers (`advance_clock`, `allow_parking`, `tick`) | `crates/gpui/src/executor.rs:174-260` |
| **worked tutorial of every pattern** | `crates/gpui/examples/testing.rs` |
| `FakeFs` | `crates/fs/src/fs.rs:1398-2070` |
| `TempTree`, marked text, `assert_set_eq!` | `crates/util/src/test.rs`, `crates/util/src/test/marked_text.rs`, `crates/util/src/test/assertions.rs` |
| `path!` / `uri!` / `line_endings!` / `#[perf]` | `crates/util_macros/src/util_macros.rs` |
| `RandomCharIter` | `crates/util/src/util.rs:578-627` |
| `EditorTestContext` | `crates/editor/src/test/editor_test_context.rs` |
| `EditorLspTestContext` | `crates/editor/src/test/editor_lsp_test_context.rs` |
| editor misc test helpers | `crates/editor/src/test.rs` |
| `FakeLanguageServer` | `crates/lsp/src/lsp.rs:1840-2090` |
| `register_fake_lsp*` | `crates/language/src/language_registry.rs:301-370` |
| `Project::test` | `crates/project/src/project.rs:1532` |
| `AppState::test` | `crates/workspace/src/workspace.rs:1149` |
| `SettingsStore::test` | `crates/settings/src/settings_store.rs:514` |
| nextest config | `.config/nextest.toml` |
| cargo aliases (`perf-test`, `perf-compare`, `xtask`) | `.cargo/config.toml` |
| perf profiler docs | `tooling/perf/src/main.rs` (top doc comment) |
| criterion benches | `crates/benchmarks/benches/`, `crates/rope/benches/`, `crates/language/benches/`, ... |
| visual/golden screenshot tests | `crates/wu/src/wu/visual_tests.rs`, `crates/wu/src/visual_test_runner.rs` |
