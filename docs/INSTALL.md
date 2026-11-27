# Installing Glint on a Mac

Glint shows what your programs did: values, logs, SQL, exceptions, timings and LLM calls from PHP,
Python, Node, the browser and WordPress, in a window on your Mac. Your coding agent (Claude Code,
Cursor and others) reads the same events. Everything stays on your machine.

## What you need

- A Mac with Apple silicon (M1 or newer) and macOS 11 or later. This build does not run on Intel
  Macs.
- For `glint()` in your code: PHP 8.0+, Python 3.9+ or Node 18+, whichever you use. Glint works
  with the versions your terminal finds (Homebrew, nvm, pyenv and so on).

## Install

1. Open `Glint-<version>-macos-arm64.dmg` and drag Glint into Applications.

2. Open Glint from Applications. The first time, macOS shows a message that it cannot verify the
   app, because this build is not signed and notarized through Apple. Click Done. Then open
   System Settings, go to Privacy & Security, scroll down to the message "Glint was blocked to
   protect your Mac", click Open Anyway and confirm with your password. You do this once.

   If you prefer Terminal, this does the same:

   ```bash
   xattr -dr com.apple.quarantine /Applications/Glint.app
   ```

   If macOS says the app "is damaged and can't be opened", the download was quarantined in the same
   way; the command above fixes it.

3. A small spark appears in the menu bar and the Glint window opens. Allow notifications when
   macOS asks. Glint uses them to tell you when a program fails or waits for you.

4. The Glint window shows what is set up on this Mac. Click **Set up this Mac** there (or
   right-click the spark and choose **Set up this Mac…**). After a few seconds the list shows a
   check mark next to each part that is ready. Open a new terminal window afterwards, so your
   shell picks up the changes. **Show me an example** fills the window with a sample run, so you can
   see what Glint looks like before you send anything yourself.

   The same from Terminal:

   ```bash
   /Applications/Glint.app/Contents/Resources/bin/glint install
   ```

## Check it

```bash
glint doctor
```

`glint doctor` says whether Glint is running, whether `glint()` works in PHP, Python and Node, and
which coding agents know about Glint. Then send something:

```bash
echo '<?php glint(["hello" => "from PHP"]);' > /tmp/hi.php && php /tmp/hi.php
python3 -c 'glint({"hello": "from Python"})'
node -e 'glint({ hello: "from Node" })'
```

Each value shows up in the Glint window. Press `?` there for the full documentation, with an
example of every method in every language, and `,` for Settings.

## What setup changes on your Mac

Setup writes these files, and `glint uninstall` removes them again:

- `~/Library/LaunchAgents/local.glint.glintd.plist`, so the Glint service starts when you log in;
- `~/.glint/` with the service, the command line tool, the SDKs, the settings and the event
  database;
- a `99-glint.ini` in PHP's configuration folder, so every PHP script has `glint()`;
- a `glint.pth` in Python's user site-packages, so every Python program has `glint()`;
- a few marked lines in your shell profile (`~/.zshrc` for zsh), so Node has `glint()` and the
  `glint` command is on your PATH;
- the MCP server entry for the coding agents it finds (Claude Code, Cursor, VS Code, Windsurf,
  Gemini CLI, Codex), and hooks in `~/.claude/settings.json` for Claude Code. Each file gets a
  one-time `.glint-backup` copy before the first change.

Glint never sends anything off your Mac. SDKs send nothing when `APP_ENV` or `NODE_ENV` is
`production`, so a production deployment stays untouched even with the same code.

## Uninstall

```bash
glint uninstall
```

Then quit Glint from its menu, move the app to the Trash, and delete `~/.glint` if you also want
the stored events and settings gone.

## When something is off

| What you see | What to do |
|---|---|
| macOS won't open the app | Follow step 2: Privacy & Security, Open Anyway, or the `xattr` command. |
| No spark in the menu bar | On a MacBook with a notch, menu bar icons can hide behind it. Quit an app with a long menu, or look for the Glint icon in the Dock instead: it shows a badge on failures. |
| No notifications | System Settings > Notifications > Glint, and allow them. |
| `glint()` is undefined | Open a new terminal window. Run `glint doctor`, which says what is missing. Code run with `php -r` never gets it; a PHP file does. |
| `glint: command not found` | Open a new terminal window, or run `/Applications/Glint.app/Contents/Resources/bin/glint`. |
| Setup reported a problem | Read `~/.glint/setup.log`. PHP's configuration folder may need `sudo` on some setups; run the install command from step 4 in Terminal to see the details. |
