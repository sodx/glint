# Installing Glint on a Mac

Glint shows what your programs did: values, logs, SQL, exceptions, timings and LLM calls from PHP,
Python, Node, the browser and WordPress, in a window on your Mac. Your coding agent (Claude Code,
Cursor and others) reads the same events. Everything stays on your machine.

## What you need

- A Mac with Apple silicon (M1 or newer) and macOS 11 or later. This build does not run on Intel
  Macs.
- For `glint()` in your code: PHP 8.0+, Python 3.9+ or Node 18.19+ (or 20.6+), whichever you use.
  Setup covers the versions your terminal finds (Homebrew, nvm, pyenv and so on); `glint doctor`
  names the other ones it finds and prints the command that covers each. An older PHP or Node is
  left alone, so it keeps working exactly as before.

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

`glint doctor` says whether Glint is running and answering, whether the login service runs it,
whether `glint()` works in PHP, Python and Node, which other PHP and Python versions (Homebrew
versions, pyenv versions, a virtual environment) do not have `glint()` yet and how to add it, and
which coding agents know about Glint. Then send something:

```bash
echo '<?php glint(["hello" => "from PHP"]);' > /tmp/hi.php && php /tmp/hi.php
python3 -c 'glint({"hello": "from Python"})'
node -e 'glint({ hello: "from Node" })'
```

Each value shows up in the Glint window. Press `?` there for the full documentation, with an
example of every method in every language, and `,` for Settings.

Code that runs in Docker, a dev container or on a machine you reach over SSH needs one more step:
run `glint docker` in the project folder and add what it prints to your compose file (see
"Docker and remote machines" in the README).

Glint updates itself: when a new version is out, the menu-bar menu shows **Update to x.y.z…** and
the window shows a banner; one click installs it, glintd and the `glint` command included, and
restarts Glint. The version you have is in the window title and in About Glint.

## What setup changes on your Mac

Setup writes these files, lists each one in `~/.glint/installed.json`, and `glint uninstall`
removes them again, even if the PHP or Python they belong to is no longer on your PATH:

- `~/Library/LaunchAgents/local.glint.glintd.plist`, so the Glint service starts when you log in.
  If glintd stops with an error, macOS starts it again after 10 seconds; it never restarts in a
  tight loop. A Glint service that finds another glintd on its port waits for that one to stop.
- `~/.glint/` with the service, the command line tool, the SDKs, the settings and the event
  database. If the database file is damaged, glintd moves it aside as `glint.db.corrupt-<time>`
  and starts with an empty one.
- a `99-glint.ini` and a small `glint-prepend.php` in PHP's configuration folder, so every PHP
  script has `glint()`. The small file loads Glint only while `~/.glint` is there and the PHP is
  8.0 or newer, so deleting `~/.glint` never breaks PHP.
- a `glint.pth` in Python's user site-packages, so every Python program has `glint()`.
- a few marked lines in your shell profile (`~/.zshrc`, or `$ZDOTDIR/.zshrc` when you set
  ZDOTDIR), so Node has `glint()` and the `glint` command is on your PATH. Node gets `glint()`
  only while `~/.glint` is there.
- the MCP server entry for the coding agents it finds (Claude Code, Cursor, VS Code, Windsurf,
  Gemini CLI, Codex), and hooks in `~/.claude/settings.json` for Claude Code. Before each change
  a file gets a timestamped `.glint-backup-<time>` copy (the newest five are kept).

Glint never sends anything off your Mac. SDKs send nothing when `APP_ENV` or `NODE_ENV` is
`production`, so a production deployment stays untouched even with the same code. WordPress sites
report only when they are development sites: `WP_ENVIRONMENT_TYPE` is `local` or `development`,
or `WP_DEBUG` is on.

When glintd is running but stuck, your programs do not wait for it: PHP gives up after half a
second and tells the next PHP processes (php-fpm's next requests) to skip Glint for 5 seconds,
and a Node program never waits more than a quarter of a second at exit.

## Uninstall

```bash
glint uninstall
```

It removes the `99-glint.ini` of every PHP on the Mac (Homebrew versions, Herd, the system's),
not only the one on your PATH. Then quit Glint from its menu, move the app to the Trash, and
delete `~/.glint` if you also want the stored events and settings gone.

## When something is off

| What you see | What to do |
|---|---|
| macOS won't open the app | Follow step 2: Privacy & Security, Open Anyway, or the `xattr` command. |
| No spark in the menu bar | On a MacBook with a notch, menu bar icons can hide behind it. Quit an app with a long menu, or look for the Glint icon in the Dock instead: it shows a badge on failures. |
| No notifications | System Settings > Notifications > Glint, and allow them. |
| `glint()` is undefined | Open a new terminal window. Run `glint doctor`, which says what is missing. Code run with `php -r` never gets it; a PHP file does. |
| `glint()` is undefined in one PHP or Python version, or in a virtual environment | Run `glint doctor`. Its coverage line names each one and the command that adds `glint()` there. |
| Node says `--import` is not allowed or unknown | That Node is older than 18.19. Setup leaves NODE_OPTIONS alone for it; if you switched Node versions afterwards, update Node or remove the glint lines from your shell profile. |
| The menu says glintd is not running | The reason follows. Glint starts its own copy when the login service does not answer; `~/.glint/glintd.log` has the details. |
| `glint: command not found` | Open a new terminal window, or run `/Applications/Glint.app/Contents/Resources/bin/glint`. |
| Claude Code says it has no Glint server | Glint was not set up yet, or the session started before setup. Click **Set up this Mac** in the Glint window, then start a new Claude Code session. An agent you install later gets Glint the next time you click it. |
| Setup reported a problem | Read `~/.glint/setup.log`. PHP's configuration folder may need `sudo` on some setups; run the install command from step 4 in Terminal to see the details. |

## Sending diagnostics

When something still does not work, click Send diagnostics in the Glint window (Settings > This
Mac, or the first-run card when a step failed), or run `glint report` in Terminal. Glint writes
one text file to your Downloads folder and shows it in Finder. Nothing is sent anywhere: send the
file yourself to whoever helps you.

The file has the versions of the app, glintd, the `glint` tool and macOS, what `glint doctor`
finds, the last `~/.glint/setup.log`, the end of glintd's log, `~/.glint/config.toml` with
secrets removed, whether Glint.app still carries the download quarantine flag, your login
shell and its PATH, and the coding agents Glint found.
