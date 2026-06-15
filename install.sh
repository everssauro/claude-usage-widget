#!/usr/bin/env bash
# One-command install for Claude Usage Widget.
#   git clone <repo> && cd claude-usage-widget && ./install.sh
# Checks prerequisites, builds the app, and installs it. Re-run any time to update.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

bold() { printf "\033[1m%s\033[0m\n" "$1"; }
ok()   { printf "  \033[32m✓\033[0m %s\n" "$1"; }
info() { printf "  \033[34m·\033[0m %s\n" "$1"; }
die()  { printf "  \033[31m✗ %s\033[0m\n" "$1" >&2; exit 1; }

OS="$(uname -s)"
bold "Claude Usage Widget — installer ($OS)"

# --- prerequisites ----------------------------------------------------------
bold "1/3  Checking prerequisites"

if [ "$OS" = "Darwin" ]; then
  xcode-select -p >/dev/null 2>&1 || die "Xcode Command Line Tools missing. Run: xcode-select --install  (then re-run this script)"
  ok "Xcode Command Line Tools"
fi

if ! command -v node >/dev/null 2>&1; then
  die "Node.js not found. Install Node 20+ (https://nodejs.org or 'brew install node'), then re-run."
fi
ok "Node $(node -v)"

if ! command -v cargo >/dev/null 2>&1; then
  info "Rust not found — installing via rustup (non-interactive)…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
command -v cargo >/dev/null 2>&1 || die "cargo still not on PATH — open a new terminal and re-run."
ok "Rust $(cargo --version | awk '{print $2}')"

if [ "$OS" = "Linux" ]; then
  if ! pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
    info "Installing Linux system deps (needs sudo)…"
    sudo apt-get update -qq && sudo apt-get install -y -qq \
      libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf libfuse2 \
      || die "Could not install GTK/WebKit deps — install them manually and re-run."
  fi
  ok "GTK/WebKit deps"
fi

# --- build ------------------------------------------------------------------
bold "2/3  Building (first build downloads crates — a few minutes)"
npm install --silent
npm run tauri build
ok "Built"

# --- install ----------------------------------------------------------------
bold "3/3  Installing"
if [ "$OS" = "Darwin" ]; then
  APP="src-tauri/target/release/bundle/macos/Claude Usage Widget.app"
  [ -d "$APP" ] || die "Build output not found at $APP"
  rm -rf "/Applications/Claude Usage Widget.app"
  cp -R "$APP" "/Applications/"
  # Unsigned build — clear the quarantine flag so Gatekeeper doesn't block it.
  xattr -dr com.apple.quarantine "/Applications/Claude Usage Widget.app" 2>/dev/null || true
  ok "Installed to /Applications"
  open "/Applications/Claude Usage Widget.app"
  bold "Done — widget launched (top-right corner)."
  info "First run: if it asks for Keychain access, click Allow."
  info "No Claude Code on this Mac? Click the mascot's settings ⚙ → Account → Sign in with Claude."
else
  BIN="src-tauri/target/release/claude-usage-widget"
  APPIMG="$(ls src-tauri/target/release/bundle/appimage/*.AppImage 2>/dev/null | head -1 || true)"
  if [ -n "$APPIMG" ]; then
    chmod +x "$APPIMG"
    ok "AppImage: $APPIMG"
    bold "Done — run it with: \"$APPIMG\""
  else
    ok "Binary: $BIN"
    bold "Done — run it with: \"$BIN\""
  fi
  info "Linux: PiP degrades to always-on-top + all-workspaces (NSPanel is macOS-only)."
fi
