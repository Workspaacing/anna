## Installing

### macOS (Apple Silicon)

1. Download `Anna-aarch64.dmg`, open it, and drag Anna into your Applications folder. Anna is not signed with an Apple Developer certificate yet, so macOS will block it the first time you open it.
2. Open Terminal and run:

   ```sh
   xattr -d com.apple.quarantine /Applications/Anna.app
   ```

3. Open Anna normally.

If you'd rather not use Terminal: open Anna once (you'll see an "Anna can't be opened" or "Apple could not verify" message), then go to **System Settings → Privacy & Security**, scroll down, and click **Open Anyway** next to Anna. Confirm with your password.

### Linux (x86_64 and aarch64)

Download `anna-linux-<arch>.tar.gz` and unpack it into `~/.local`:

```sh
tar -xzf anna-linux-$(uname -m).tar.gz -C ~/.local
ln -sf ~/.local/anna.app/bin/anna ~/.local/bin/anna
```

Make sure `~/.local/bin` is on your `PATH`, then run `anna`.

### Windows (x86_64)

Download and run `Anna-x86_64.exe`. The installer isn't code-signed, so Windows SmartScreen may warn you. Click **More info**, then **Run anyway**.

The `anna-remote-server-*` files are used by Anna's remote development feature. You don't need to download them yourself.
