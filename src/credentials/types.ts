/**
 * Mirror of the Rust `CredentialState` enum.
 *
 * Note what is absent: the token. The Rust side cannot serialize it — the
 * wrapper type deliberately has no `Serialize` impl — so there is no shape of
 * this type that could ever carry a credential into the webview.
 */
export type CredentialState =
  | { kind: "ok"; expiresAtMs: number }
  | { kind: "expired"; expiredAtMs: number }
  | { kind: "missing" }
  | { kind: "denied" }
  | { kind: "unreadable"; reason: string };
