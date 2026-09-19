<p align="center">
  <img src="docs/images/app-icon.png" width="128" alt="AI reactor icon">
</p>

<h1 align="center">AI reactor</h1>

<p align="center">
  How much of your <b>Claude</b> and <b>Codex</b> limits is left — as two small gauges in the macOS menu bar.
</p>

<p align="center">
  <img src="docs/images/menubar.png" width="240" alt="Menu bar: Claude and Codex gauges">
</p>

- **One gauge per agent.** The arc shows how much of your **current session** is left; the logo in the middle says which agent it is.
- **Click for details.** The popover shows every limit window (5-hour, weekly, monthly), when each resets, your account, and your plan.
- **Stays out of the way.** Menu bar only — no Dock icon. It checks every 3 minutes in the background and every 30 seconds while the popover is open.
- **Local only.** Nothing is sent anywhere except the one request Claude Code itself uses to show `/usage`. No telemetry, no server.

> AI reactor is an unofficial, non-commercial project. It is not affiliated with or endorsed by Anthropic or OpenAI. "Claude" and "ChatGPT/Codex" and their logos belong to their owners and are used here only to show which service a gauge refers to.

## Requirements

- macOS 13 or later (Apple Silicon or Intel)
- **Claude:** [Claude Code](https://docs.anthropic.com/en/docs/claude-code) installed and logged in with a Claude subscription (`claude` → `/login`)
- **Codex:** the [Codex CLI](https://github.com/openai/codex), used at least once — the gauge reads its local session logs

You can use either one alone; the other gauge simply does not appear.

## Install

1. Download `AI.reactor_x.y.z_universal.dmg` from [**Releases**](https://github.com/jen454/AI-reactor/releases/latest).
2. Open it and drag **AI reactor** into **Applications**.
3. Open **AI reactor** from Applications. macOS will block it the first time, because the app is not signed with a paid Apple Developer ID:
   - Go to **System Settings → Privacy & Security**, scroll down, and click **Open Anyway** next to the message about AI reactor. (On recent macOS, right-click → Open no longer works for this.)
   - Or, in Terminal:
     ```sh
     xattr -dr com.apple.quarantine "/Applications/AI reactor.app"
     ```

## First run

A gauge appears in the menu bar, and macOS asks:

> **"AI reactor" wants to use your confidential information stored in "Claude Code-credentials" in your keychain.**

Click **Always Allow**. That's it — from then on it tracks automatically.

- **Allow** (without "Always") only lasts until the app restarts, so you would be asked again.
- If you click **Deny**, the Claude card says so, and the app asks again a little later.
- **After each update** macOS asks once more. It remembers unsigned apps by their exact build, so every new version is "new" to it. Click **Always Allow** again.
- Codex needs no permission.

Optional: right-click the menu bar icon → **로그인 시 자동 실행** (launch at login) so tracking survives a restart.

## What it reads

| What | Where | Why |
|---|---|---|
| Claude Code's login token | Keychain item `Claude Code-credentials`, or `~/.claude/.credentials.json` if there is no keychain item (read only) | To ask Anthropic for your usage, the same request `claude` makes for `/usage` |
| Your Claude account email and plan | `~/.claude.json` → `oauthAccount` (read only) | Shown in the popover |
| Codex limits and plan | `~/.codex/sessions/**/rollout-*.jsonl` (read only) | Codex writes its limits into these logs; no network needed |

And what it never does:

- **Never refreshes or modifies your tokens.** The token belongs to `claude`; refreshing it from here would log the CLI out. When it expires, the card asks you to run `claude` once.
- **Never writes** to `~/.claude` or `~/.codex`.
- **Never reads** `~/.codex/auth.json`. That's why the Codex card shows your plan but not your email.
- Keeps the token in memory only — never in a file or a log.

The only network request is `GET https://api.anthropic.com/api/oauth/usage`.

## Good to know

- The Claude usage endpoint is **undocumented**. It is what Claude Code itself uses, but it can change without notice; if it does, the Claude card shows the last known numbers as stale until the app is updated.
- Codex numbers come from the Codex CLI's logs, so they update when you use the CLI. The IDE extension doesn't write these logs.
- The menu bar gauge always shows the **current session** (the shortest window). Exact percentages, weekly and monthly limits are in the popover.

## Build from source

```sh
# Prerequisites: Rust (rustup), Node 22+, pnpm
git clone https://github.com/jen454/AI-reactor.git
cd AI-reactor
pnpm install
pnpm tauri build --bundles app
open "src-tauri/target/release/bundle/macos/AI reactor.app"
```

The build is ad-hoc signed, so the keychain prompt returns after every rebuild. For a stable local signing identity, run `scripts/setup-dev-cert.sh` once, then use `scripts/rebuild-and-run.sh`.

Tests: `cd src-tauri && cargo test`

Design notes and the history of decisions (in Korean): [`docs/SPEC.md`](docs/SPEC.md), [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md).

## 한국어 요약

메뉴바에서 Claude와 Codex의 **현재 세션 남은 한도**를 게이지로 보여주는 앱입니다.

1. [Releases](https://github.com/jen454/AI-reactor/releases/latest)에서 DMG를 받아 응용 프로그램 폴더로 옮깁니다.
2. 처음 열 때 막히면 **시스템 설정 → 개인정보 보호 및 보안 → "그래도 열기"**를 누릅니다.
3. 키체인 창이 뜨면 **"항상 허용"**을 누릅니다. 이후엔 자동으로 추적됩니다. (새 버전을 설치하면 한 번 더 물어봅니다.)

토큰은 읽기만 하고 갱신·저장하지 않으며, Anthropic 사용량 조회 외에는 어떤 네트워크 요청도 하지 않습니다. Anthropic·OpenAI와 무관한 비공식·비상업 프로젝트입니다.

## License

[MIT](LICENSE)
