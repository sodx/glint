#!/bin/bash
# Acts like a brand new tester: takes a Glint disk image (or Glint.app), copies the app out of it,
# puts the browser's download quarantine flag on every file, starts Glint, presses "Set up this
# Mac" (POST /v1/setup), and checks what testers depend on:
#
#   - every setup step is ok, or a skip with a reason;
#   - glint() from PHP, Python and Node reaches glintd;
#   - Claude Code has the MCP server and the hooks;
#   - an agent is found when only its app (Windsurf.app) or only its command (gemini) exists;
#   - Glint.app opens at login (a LaunchAgent that names this copy of the app);
#   - glint report writes a readable file;
#   - glint uninstall removes what install wrote, and a running Glint.app quits (--launch-app).
#
#   scripts/clean-mac-check.sh                    # sandbox: a fresh HOME in a temp folder
#   scripts/clean-mac-check.sh --app Glint.app    # a bundle or .dmg (default: newest in target/release)
#   scripts/clean-mac-check.sh --ci               # the real HOME, /Applications and port 23600 (CI only)
#   scripts/clean-mac-check.sh --keep             # keep the sandbox for a look afterwards
#
# The sandbox never touches the real setup: HOME, the Applications folder, PHP's ini scan
# directory and glintd's port are all its own, and GLINT_SKIP_SERVICE_START keeps launchd out of
# it. Results (setup JSON, setup.log, doctor output, the report) go to $GLINT_CHECK_OUT, default
# <sandbox>/out. Needs php, python3, node and claude on PATH; a missing one is a failure.
set -u

ci=0 keep=0 launch_app=0 app_in=""
while [ $# -gt 0 ]; do
  case "$1" in
    --ci) ci=1 ;;
    --keep) keep=1 ;;
    --launch-app) launch_app=1 ;;
    --app) app_in="$2"; shift ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

root=$(cd "$(dirname "$0")/.." && pwd)
failures=0
pass() { printf '  ok    %s\n' "$*"; }
fail() { printf '  FAIL  %s\n' "$*"; failures=$((failures + 1)); }
info() { printf '  ..    %s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
need() { command -v "$1" >/dev/null 2>&1 || { echo "needs $1 on PATH" >&2; exit 2; }; }
for t in curl hdiutil xattr; do need "$t"; done

# ------------------------------------------------------------------ where things go

if [ "$ci" = 1 ]; then
  sandbox="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/glint-clean-mac"
  rm -rf "$sandbox"; mkdir -p "$sandbox"
  home="$HOME"
  apps=/Applications
  port=23600
else
  sandbox=$(mktemp -d "${TMPDIR:-/tmp}/glint-clean-mac.XXXXXX")
  home="$sandbox/home"
  apps="$home/Applications"
  port=""
  for p in $(seq 23710 23790); do
    if ! lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then port=$p; break; fi
  done
  [ -n "$port" ] || { echo "no free port in 23710-23790" >&2; exit 2; }
fi
out="${GLINT_CHECK_OUT:-$sandbox/out}"
mkdir -p "$home" "$apps" "$out"
url="http://127.0.0.1:$port"
app="$apps/Glint.app"
pids=""

cleanup() {
  for p in $pids; do kill "$p" 2>/dev/null; done
  # Whatever else still runs from this copy of the app (the glintd Glint.app started).
  pkill -f "$app/Contents/" 2>/dev/null
  if [ "$ci" = 0 ] && [ "$keep" = 0 ]; then
    rm -rf "$sandbox"
  elif [ "$ci" = 0 ]; then
    echo "sandbox kept: $sandbox"
  fi
}
trap cleanup EXIT

shell=$(dscl . -read "/Users/$USER" UserShell 2>/dev/null | awk '{print $2}')
[ -x "${shell:-}" ] || shell=/bin/zsh
case "$shell" in
  */zsh) profile="$home/.zprofile" ;;
  */bash) profile="$home/.bash_profile" ;;
  *) profile="$home/.profile" ;;
esac

echo "mode:     $([ "$ci" = 1 ] && echo "ci (real HOME)" || echo sandbox)"
echo "HOME:     $home"
echo "glintd:   $url"
echo "shell:    $shell (tools on PATH via $profile)"
echo "results:  $out"

# The environment glintd has when the app starts it: next to nothing. Everything else comes from
# the login shell, the way a tester's Mac works.
base_env=(HOME="$home" USER="$USER" LOGNAME="$USER" SHELL="$shell" TERM=dumb LANG=en_US.UTF-8
  PATH=/usr/bin:/bin:/usr/sbin:/sbin TMPDIR="${TMPDIR:-/tmp}" GLINT_URL="$url")
if [ "$ci" = 0 ]; then
  mkdir -p "$home/php-conf.d"
  base_env+=(GLINT_SKIP_SERVICE_START=1 PHP_INI_SCAN_DIR="$home/php-conf.d")
fi
clean() { env -i "${base_env[@]}" "$@"; }
# A command as the tester's terminal runs it: a login, interactive shell.
in_shell() { clean "$shell" -lic "$1" </dev/null 2>&1; }

# ------------------------------------------------------------------ the app

step "Glint.app from the disk image"
if [ -z "$app_in" ]; then
  app_in=$(ls -t "$root"/target/release/Glint-*.dmg 2>/dev/null | head -1)
  [ -n "$app_in" ] || app_in="$root/target/release/Glint.app"
fi
[ -e "$app_in" ] || { echo "no Glint disk image or app at $app_in; build one with app/bundle-macos.sh --dmg" >&2; exit 2; }
rm -rf "$app"
case "$app_in" in
  *.dmg)
    mnt="$sandbox/mnt"; mkdir -p "$mnt"
    hdiutil attach -quiet -nobrowse -readonly -mountpoint "$mnt" "$app_in" || { echo "could not mount $app_in" >&2; exit 2; }
    cp -R "$mnt/Glint.app" "$app"; cp_status=$?
    [ -f "$mnt/Read me first.txt" ] && pass "disk image has Read me first.txt" || fail "disk image has no Read me first.txt"
    [ -L "$mnt/Applications" ] && pass "disk image links to /Applications" || fail "disk image has no /Applications link"
    hdiutil detach -quiet "$mnt" || hdiutil detach -quiet -force "$mnt"
    [ "$cp_status" = 0 ] || { echo "copy failed" >&2; exit 2; } ;;
  *) cp -R "$app_in" "$app" ;;
esac
pass "copied to $app"
for f in Contents/MacOS/Glint Contents/MacOS/glintd Contents/Resources/bin/glint Contents/Info.plist; do
  [ -e "$app/$f" ] && pass "bundle has $f" || fail "bundle has no $f"
done

step "Quarantine, as a browser download leaves it"
flag="0083;$(printf %x "$(date +%s)");Safari;$(uuidgen)"
find "$app" -exec xattr -w com.apple.quarantine "$flag" {} \; 2>/dev/null
if xattr -p com.apple.quarantine "$app/Contents/Resources/bin/glint" >/dev/null 2>&1; then
  pass "every file carries com.apple.quarantine"
else
  fail "could not set the quarantine flag"
fi
# "Open Anyway" in Privacy & Security marks the bundle folder as approved (0x40 in the flag's
# first field); the files inside keep their flag, and macOS judges a program inside an app by
# its bundle's flag. Approve before running anything inside: once Gatekeeper has refused the
# bundle, macOS may protect it from further changes (App Management).
approved="00c3;${flag#*;}"
if xattr -w com.apple.quarantine "$approved" "$app"; then
  pass "bundle approved as by Open Anyway; files inside keep their flag"
else
  fail "could not mark the bundle approved"
fi

# ------------------------------------------------------------------ a tester's profile

step "A tester's login profile"
fakebin="$sandbox/fakebin"
mkdir -p "$fakebin"
# Only the command: Gemini CLI installed, never run (no ~/.gemini yet).
printf '#!/bin/sh\necho "gemini 0.0.0 (clean-mac-check stand-in)"\n' > "$fakebin/gemini"
chmod +x "$fakebin/gemini"
# Only the app: Windsurf in ~/Applications, never opened (no ~/.codeium yet).
mkdir -p "$home/Applications/Windsurf.app/Contents"
dirs="$fakebin"
for t in php python3 node claude; do
  p=$(command -v "$t" 2>/dev/null) || { fail "$t is not on PATH here; install it first"; continue; }
  d=$(dirname "$p")
  case ":$dirs:" in *":$d:"*) ;; *) dirs="$dirs:$d" ;; esac
done
{
  echo "# clean-mac-check: the tools a tester has (Homebrew, nvm, ...)"
  echo "export PATH=\"$dirs:\$PATH\""
} >> "$profile"
pass "PATH in $profile: $dirs"
for d in .gemini .codeium/windsurf; do
  [ -e "$home/$d" ] && info "~/$d exists already: agent detection by app or command is not what finds it"
done

# ------------------------------------------------------------------ start Glint

step "Start Glint"
mkdir -p "$home/.glint"
up() { curl -H "X-Glint: 1" -fsS -m 2 "$url/v1/info" >/dev/null 2>&1; }
wait_up() { for _ in $(seq 1 "$1"); do up && return 0; sleep 1; done; return 1; }
if up; then echo "something already answers at $url; stop it first" >&2; exit 2; fi
if [ "$launch_app" = 1 ]; then
  # The app clears the flag on its bundle and starts the bundled glintd.
  # Its own identifier, so it runs beside a Glint already open on this Mac instead of handing over.
  env -i "${base_env[@]}" GLINT_APP_ID=local.glint.app.check "$app/Contents/MacOS/Glint" > "$out/app.log" 2>&1 &
  app_pid=$!
  pids="$pids $app_pid"
  if wait_up 40; then pass "Glint.app started glintd"; else fail "Glint.app did not start glintd (app.log follows)"; tail -30 "$out/app.log"; fi
else
  # What the app does when nothing answers: run the bundled glintd, logging to ~/.glint/glintd.log.
  extra=()
  [ "$ci" = 0 ] && extra=(GLINT_ADDR="127.0.0.1:$port" GLINT_OTLP_ADDR=off)
  env -i "${base_env[@]}" ${extra[@]+"${extra[@]}"} "$app/Contents/MacOS/glintd" >> "$home/.glint/glintd.log" 2>&1 &
  pids="$pids $!"
  if wait_up 20; then pass "bundled glintd answers at $url"; else fail "bundled glintd did not start (glintd.log follows)"; tail -30 "$home/.glint/glintd.log"; fi
fi
up || { echo "glintd is not running; nothing more to check" >&2; exit 1; }
# A tester whose app could not clear the flags (no write access to /Applications) still has them.
# Setup has to work anyway.
if xattr -w com.apple.quarantine "$approved" "$app" 2>/dev/null && xattr -w com.apple.quarantine "$flag" "$app/Contents/Resources/bin/glint" 2>/dev/null; then
  info "flags put back, as when the app cannot clear them"
else
  info "macOS protects the started app from changes; its flags stay as they are"
fi

# ------------------------------------------------------------------ set up this Mac

# Lines `<status> <name> <detail>` from the JSON of /v1/setup.
steps_of() { /usr/bin/python3 -c 'import json,sys
d=json.load(sys.stdin)
for s in d.get("steps",[]): print(s["status"], s["name"], s["detail"].replace("\n"," "))' < "$1"; }

step "Before setup: GET /v1/setup"
curl -H "X-Glint: 1" -sS -m 60 "$url/v1/setup" -o "$out/setup-before.json" -w '%{http_code}\n' > "$out/status" 2>&1
[ "$(cat "$out/status")" = 200 ] && pass "GET /v1/setup answers" || { fail "GET /v1/setup: HTTP $(cat "$out/status"): $(cat "$out/setup-before.json")"; }
steps_of "$out/setup-before.json" 2>/dev/null | sed 's/^/        /'

step "Set up this Mac: POST /v1/setup"
curl -H "X-Glint: 1" -sS -m 300 -X POST "$url/v1/setup" -o "$out/setup.json" -w '%{http_code}\n' > "$out/status" 2>&1
if [ "$(cat "$out/status")" = 200 ]; then
  pass "POST /v1/setup answers"
  steps_of "$out/setup.json" > "$out/setup-steps.txt"
  while read -r status name detail; do
    case "$status" in
      ok) pass "$name: $detail" ;;
      skip) info "$name (skip): $detail" ;;
      *) fail "$name: $detail" ;;
    esac
  done < "$out/setup-steps.txt"
  for want in service login php python node claude; do
    grep -q "^ok $want " "$out/setup-steps.txt" || fail "setup did not report $want as ok"
  done
  login_plist="$home/Library/LaunchAgents/local.glint.app.login.plist"
  if plutil -lint "$login_plist" >/dev/null 2>&1 && grep -qF "<string>$app</string>" "$login_plist"; then
    pass "the login item opens $app"
  else
    fail "no valid login item for $app: $(cat "$login_plist" 2>&1 | head -5)"
  fi
  grep -q "^ok agents Windsurf" "$out/setup-steps.txt" && pass "Windsurf found by its app alone" || fail "Windsurf (only Windsurf.app) was not set up"
  grep -q "^ok agents Gemini CLI" "$out/setup-steps.txt" && pass "Gemini CLI found by its command alone" || fail "Gemini CLI (only the gemini command) was not set up"
else
  fail "POST /v1/setup: HTTP $(cat "$out/status"): $(head -c 2000 "$out/setup.json")"
fi
cp "$home/.glint/setup.log" "$out/setup.log" 2>/dev/null && pass "~/.glint/setup.log written" || fail "no ~/.glint/setup.log"

step "After setup: GET /v1/setup (doctor)"
curl -H "X-Glint: 1" -sS -m 60 "$url/v1/setup" -o "$out/setup-after.json"
steps_of "$out/setup-after.json" > "$out/doctor-after.txt" 2>&1
while read -r status name detail; do
  case "$status" in
    ok) pass "$name: $detail" ;;
    skip) info "$name (skip): $detail" ;;
    *) fail "$name: $detail" ;;
  esac
done < "$out/doctor-after.txt"
for want in daemon service login php python node agents; do
  grep -q "^ok $want " "$out/doctor-after.txt" || fail "doctor does not report $want as ok"
done

# ------------------------------------------------------------------ glint() lands

step "glint() from each language reaches glintd"
mark="cm$$x$RANDOM"
lands() {
  for _ in $(seq 1 20); do
    curl -H "X-Glint: 1" -fsS -m 2 "$url/v1/events?q=$1&limit=5" 2>/dev/null | grep -q "$1" && return 0
    sleep 0.5
  done
  return 1
}
in_shell "echo '<?php glint(\"${mark}php\");' > \"\$HOME/hi.php\" && php \"\$HOME/hi.php\"" > "$out/php.txt"
lands "${mark}php" && pass "PHP glint() arrived" || { fail "PHP glint() did not arrive"; cat "$out/php.txt"; }
in_shell "python3 -c 'glint(\"${mark}python\")'" > "$out/python.txt"
lands "${mark}python" && pass "Python glint() arrived" || { fail "Python glint() did not arrive"; cat "$out/python.txt"; }
in_shell "node -e 'glint(\"${mark}node\")'" > "$out/node.txt"
lands "${mark}node" && pass "Node glint() arrived" || { fail "Node glint() did not arrive"; cat "$out/node.txt"; }

# ------------------------------------------------------------------ Claude Code

step "Claude Code knows Glint"
in_shell "claude mcp list" > "$out/claude-mcp-list.txt"
grep -q "^glint" "$out/claude-mcp-list.txt" && pass "claude mcp list has glint" || { fail "claude mcp list has no glint"; cat "$out/claude-mcp-list.txt"; }
grep -q "\"glint\"" "$home/.claude.json" 2>/dev/null && grep -q "$url/mcp" "$home/.claude.json" && pass "~/.claude.json points glint at $url/mcp" || fail "~/.claude.json has no glint server for $url/mcp"
grep -q "glint hook" "$home/.claude/settings.json" 2>/dev/null && pass "hooks in ~/.claude/settings.json" || fail "no glint hook in ~/.claude/settings.json"

step "Agents found by app or command"
grep -q '"glint"' "$home/.codeium/windsurf/mcp_config.json" 2>/dev/null && pass "Windsurf: ~/.codeium/windsurf/mcp_config.json" || fail "Windsurf has no glint server"
grep -q '"glint"' "$home/.gemini/settings.json" 2>/dev/null && pass "Gemini CLI: ~/.gemini/settings.json" || fail "Gemini CLI has no glint server"

# ------------------------------------------------------------------ diagnostics

step "glint report"
in_shell "\"\$HOME/.glint/bin/glint\" report --no-reveal --out \"$out\"" > "$out/report-run.txt"
report=$(ls -t "$out"/glint-report-*.txt 2>/dev/null | head -1)
if [ -n "$report" ]; then
  pass "wrote $(basename "$report")"
  for s in "== Versions ==" "== Quarantine ==" "== Doctor" "== Coding agents found ==" "== Login shell ==" "== Last setup" "== glintd log" "== config.toml"; do
    grep -qF "$s" "$report" && pass "report has $s" || fail "report has no $s"
  done
  grep -q "^Windsurf .*app " "$report" && pass "report says how Windsurf was found" || fail "report does not list Windsurf's app"
else
  fail "glint report wrote nothing"; cat "$out/report-run.txt"
fi
if [ "$ci" = 1 ]; then
  # The window's button; it also shows the file in Finder, which is harmless on a runner.
  curl -H "X-Glint: 1" -sS -m 120 -X POST "$url/v1/report" -o "$out/report.json"
  grep -q '"complete":true' "$out/report.json" && pass "POST /v1/report: $(cat "$out/report.json")" || fail "POST /v1/report: $(cat "$out/report.json")"
fi

# ------------------------------------------------------------------ uninstall

step "glint uninstall"
in_shell "\"\$HOME/.glint/bin/glint\" uninstall" > "$out/uninstall.txt"
sed 's/^/        /' "$out/uninstall.txt"
[ ! -e "$home/Library/LaunchAgents/local.glint.glintd.plist" ] && pass "LaunchAgent removed" || fail "LaunchAgent still there"
[ ! -e "$home/Library/LaunchAgents/local.glint.app.login.plist" ] && pass "login item removed" || fail "login item still there"
[ -e "$home/.glint/uninstalled" ] && pass "uninstall left the marker a running Glint.app quits on" || fail "no ~/.glint/uninstalled"
if [ -n "${app_pid:-}" ]; then
  for _ in $(seq 1 15); do kill -0 "$app_pid" 2>/dev/null || break; sleep 1; done
  kill -0 "$app_pid" 2>/dev/null && fail "Glint.app still runs after glint uninstall" || pass "Glint.app quit after glint uninstall"
fi
ini_dir=$(in_shell "php --ini" | sed -n 's/^Scan for additional .ini files in: //p' | cut -d: -f1)
[ -n "$ini_dir" ] && [ ! -e "$ini_dir/99-glint.ini" ] && pass "PHP: no 99-glint.ini in $ini_dir" || fail "PHP: 99-glint.ini still in ${ini_dir:-?}"
site=$(in_shell "python3 -c 'import site; print(site.getusersitepackages())'" | tail -1)
[ ! -e "$site/glint.pth" ] && pass "Python: no glint.pth in $site" || fail "Python: glint.pth still in $site"
rc=$(in_shell 'echo "$NODE_OPTIONS"' | tail -1)
grep -q ">>> glint >>>" "$home/.zshrc" "$home/.bash_profile" "$home/.bashrc" 2>/dev/null && fail "a shell profile still has the glint block" || pass "no glint block in shell profiles"
case "$rc" in *glint*) fail "NODE_OPTIONS still loads glint: $rc" ;; *) pass "NODE_OPTIONS no longer loads glint" ;; esac
in_shell "claude mcp get glint" > "$out/claude-after.txt"
grep -q "$url/mcp" "$out/claude-after.txt" && fail "Claude Code still has glint" || pass "Claude Code: glint removed"
grep -q "glint hook" "$home/.claude/settings.json" 2>/dev/null && fail "hooks still in ~/.claude/settings.json" || pass "hooks removed from ~/.claude/settings.json"
grep -q '"glint"' "$home/.codeium/windsurf/mcp_config.json" 2>/dev/null && fail "Windsurf still has glint" || pass "Windsurf: glint removed"
grep -q '"glint"' "$home/.gemini/settings.json" 2>/dev/null && fail "Gemini CLI still has glint" || pass "Gemini CLI: glint removed"
in_shell "\"\$HOME/.glint/bin/glint\" doctor" > "$out/doctor-after-uninstall.txt"

step "Result"
if [ "$failures" = 0 ]; then
  echo "PASS: a fresh Mac gets a working Glint"
  exit 0
fi
echo "FAIL: $failures check(s) failed; details in $out"
[ "$ci" = 0 ] && [ "$keep" = 0 ] && cp -R "$out" "${TMPDIR:-/tmp}/glint-clean-mac-last" && echo "copied to ${TMPDIR:-/tmp}/glint-clean-mac-last"
exit 1
