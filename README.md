> [!IMPORTANT]
> Remove this line to confirm you've reviewed this PR before submitting.
<div align="center">
  <img src="crates/wu/resources/app-icon.png" alt="Wu" width="128">
  <h1>Wu</h1>
  <p>The fast, native code editor that doesn't get in your way.</p>
  <p><a href="https://github.com/Workspaacing/wu/releases/latest"><strong>Download</strong></a></p>
</div>

---

Wu is a code editor for people who want the speed of a native app and the familiarity of VS Code. The name comes from [wu wei](https://en.wikipedia.org/wiki/Wu_wei): effortless action.

Wu is a fork of [Zed](https://github.com/zed-industries/zed). It inherits Zed's editor core, GPU rendering, and language tooling, and drops the rest: no collaboration, no channels, no accounts, no telemetry.

## Features

- **Native and fast.** Written in Rust, rendered on the GPU. No Electron or webviews. Opens instantly and stays responsive on large views.
- **Lightweight.** Base memory usage is even lower than Zed.
- **AI on your own terms.** Cowork gives you chat threads against any model in the [models.dev](https://models.dev) catalog. There is no Wu account and no Wu proxy: requests go straight to the provider you picked, using a key Wu reads from your environment and never stores. Prefer an external agent or harness? Nothing stops you — Cowork is a panel you can ignore or hide.
- **Feels like VS Code out of the box.** UI and defaults are tuned so you don't have to relearn your editor.
- **No account or telemetry.** Nothing to sign in to. Wu never sends your usage data anywhere.

## Cowork

Cowork is Wu's AI surface, split the way the rest of the workspace is: a dock panel holds your
session history, search and provider status, while each conversation opens as an ordinary tab in
the center, next to the code it's about.

To use it:

1. Export the API key for a provider you already have, using the variable name
   [models.dev](https://models.dev) lists for it — for example `ANTHROPIC_API_KEY` or
   `OPENAI_API_KEY` — and start Wu from that environment.
2. Press <kbd>ctrl-alt-a</kbd> (<kbd>cmd-ctrl-a</kbd> on macOS) to open the panel, then
   <kbd>ctrl-alt-n</kbd> (<kbd>cmd-ctrl-t</kbd>) to start a thread.
3. Pick a model from the header of the thread. Threads are stored locally and are searchable from
   the panel.

Cowork is plain chat today: it reads no files, runs no commands, and makes no tool calls. Set
`"cowork": { "button": false }` in your settings to hide it from the activity bar.

---

![Wu screenshot dark](assets/images/screenshot-dark.png)

![Wu screenshot light](assets/images/screenshot-light.png)

## Docs

See [docs](https://wu.farshed.me).

## Install

Download the installer for your platform [here](https://github.com/Workspaacing/wu/releases/latest). Then follow the steps below.

### macOS (Apple Silicon)

1. Download `Wu-aarch64.dmg`, open it, and drag Wu into your Applications folder. Wu is not signed with an Apple Developer certificate yet, so macOS will block it the first time you open it.
2. Open Terminal and run:

   ```sh
   xattr -d com.apple.quarantine /Applications/Wu.app
   ```

3. Open Wu normally.

If you'd rather not use Terminal: open Wu once (you'll see a "Wu can't be opened" or "Apple could not verify" message), then go to **System Settings → Privacy & Security**, scroll down, and click **Open Anyway** next to Wu. Confirm with your password.

### Linux (x86_64 and aarch64)

Download `wu-linux-<arch>.tar.gz` and unpack it into `~/.local`:

```sh
tar -xzf wu-linux-$(uname -m).tar.gz -C ~/.local
ln -sf ~/.local/wu.app/bin/wu ~/.local/bin/wu
```

Make sure `~/.local/bin` is on your `PATH`, then run `wu`.

### Windows (x86_64)

Download and run `Wu-x86_64.exe`. The installer isn't code-signed, so Windows SmartScreen may warn you. Click **More info**, then **Run anyway**.

## Building

Wu builds the same way Zed does. See the [Zed development docs](https://zed.dev/docs/development).

## Licensing

Wu is licensed under GPL-3.0-or-later with Apache-2.0 components where marked. See [LICENSE-GPL](./LICENSE-GPL) and [LICENSE-APACHE](./LICENSE-APACHE).

Wu is a derivative work of [Zed](https://github.com/zed-industries/zed) and shares the same licenses.

## Acknowledgements

Thanks to the Zed team for building an excellent editor and releasing it as open source. Wu would not exist without their work.
