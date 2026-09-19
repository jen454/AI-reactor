#!/usr/bin/env bash
# Rebuild the .app and actually run the new one.
#
# Two things make "just run it again" unreliable, and both bit during
# development:
#
#   1. `open` does not restart a running app — it activates the existing
#      process. So you can stare at a build from ten minutes ago and conclude
#      your change did nothing.
#
#   2. WKWebView caches `index.html`. Vite hashes the asset filenames, so the
#      JS and CSS are cache-safe, but the document that *points* at them is
#      not: a cached index.html keeps requesting the previous build's hashes.
#
# So: quit for real, clear the webview cache, rebuild, launch.

set -euo pipefail

cd "$(dirname "$0")/.."

APP="src-tauri/target/release/bundle/macos/AI reactor.app"
BUNDLE_ID="dev.jinwook.aireactor"

echo "==> 실행 중인 AI reactor 종료"
pkill -f 'AI reactor.app/Contents/MacOS/ai-reactor' 2>/dev/null || true
sleep 1

echo "==> webview 캐시 삭제"
# App-owned caches only. Nothing here is user data; WebKit recreates them.
rm -rf "$HOME/Library/Caches/$BUNDLE_ID" "$HOME/Library/WebKit/$BUNDLE_ID"

echo "==> 빌드"
# tauri.local.conf.json (gitignored) holds a personal signing identity — see
# scripts/setup-dev-cert.sh. Without it the build is ad-hoc signed, which
# works but re-asks for keychain access after every rebuild.
LOCAL_CONF="src-tauri/tauri.local.conf.json"
if [[ -f "$LOCAL_CONF" ]]; then
  pnpm tauri build --bundles app --config "$LOCAL_CONF"
else
  pnpm tauri build --bundles app
fi

echo "==> 실행"
open "$APP"

echo
echo "완료. 메뉴바에서 AI reactor를 확인하세요."
echo "(Rust를 고쳤다면 키체인 허용 창이 한 번 뜹니다 — docs/DEVELOPMENT.md의 '개발 중 키체인 허용 창' 참고)"
