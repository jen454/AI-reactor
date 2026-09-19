#!/usr/bin/env bash
# Create a local code-signing identity for AI reactor development.
#
# Why this exists
# ---------------
# AI reactor reads the `claude` CLI's credential out of the login keychain. That
# keychain item has an ACL, and macOS matches an app against it by the app's
# *code signature*. An unsigned (or ad-hoc signed) build gets a fresh identity
# every time it is compiled, so the "Always Allow" you clicked last build no
# longer matches and the permission dialog comes back — every single rebuild.
#
# Signing with a stable certificate fixes *half* of that, and it is worth
# knowing which half.
#
#   Fixed:     the ACL's requirement entry. It becomes
#              `identifier "dev.jinwook.aireactor" and certificate root = H"..."`,
#              which is stable across rebuilds.
#
#   NOT fixed: macOS also keeps a separate "partition list" on the keychain
#              item, and that is checked independently. For an app with a Team
#              ID it records `teamid:XXXXXXXXXX`; a self-signed certificate has
#              no Team ID, so macOS falls back to recording the binary's
#              `cdhash:` — which changes on every rebuild. So the dialog still
#              appears once per rebuild of the Rust side.
#
# The real fix is an Apple Development certificate (free with any Apple ID, via
# Xcode → Settings → Accounts). That has a Team ID, so the partition entry
# survives rebuilds. Until then, this script buys a stable app identity — the
# groundwork — but not silence.
#
# Run once per machine. Safe to re-run — it replaces the existing identity.

set -euo pipefail

# Override with DEV_CERT_NAME if you already have one under another name.
NAME="${DEV_CERT_NAME:-AI reactor Dev}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

echo "==> 자체 서명 코드 서명 인증서 생성: $NAME"
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout "$WORKDIR/dev.key" -out "$WORKDIR/dev.crt" -days 3650 \
  -subj "/CN=$NAME/O=AI reactor/C=KR" \
  -addext "basicConstraints=critical,CA:false" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" 2>/dev/null

# macOS `security import` cannot read OpenSSL 3's default PKCS#12 encryption
# (AES-256 / SHA-256). Force the legacy 3DES + SHA-1 combination it accepts.
echo "==> PKCS#12로 묶는 중 (macOS가 읽을 수 있는 레거시 형식)"
openssl pkcs12 -export -out "$WORKDIR/dev.p12" \
  -inkey "$WORKDIR/dev.key" -in "$WORKDIR/dev.crt" \
  -name "$NAME" -passout pass:ai-reactor \
  -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1 -legacy 2>/dev/null

echo "==> 로그인 키체인에 가져오는 중"
# -T grants codesign access to the private key without a prompt on each build.
security import "$WORKDIR/dev.p12" -k "$KEYCHAIN" -P ai-reactor -T /usr/bin/codesign -A

echo "==> src-tauri/tauri.local.conf.json 작성 (gitignore 대상)"
cat > "$(dirname "$0")/../src-tauri/tauri.local.conf.json" <<JSON
{
  "bundle": {
    "macOS": {
      "signingIdentity": "$NAME"
    }
  }
}
JSON

echo
echo "완료. 확인:"
security find-identity -p codesigning | grep "$NAME" || true
echo
echo "인증서는 신뢰되지 않은(self-signed) 상태로 남습니다 — 서명에는 문제가 없고,"
echo "배포용이 아니라 빌드 간 앱 ID를 고정하는 것이 목적입니다."
echo
echo "주의: Rust를 리빌드할 때마다 키체인 허용 창은 계속 뜹니다."
echo "자체 서명에는 Team ID가 없어서 macOS가 partition list에 cdhash를 기록하고,"
echo "cdhash는 빌드마다 바뀌기 때문입니다. 완전히 없애려면 Apple Development"
echo "인증서(무료 Apple ID로 Xcode에서 발급)가 필요합니다."
