import claudeIcon from "../assets/claude.svg";
import codexIcon from "../assets/codex.svg";
import { Gauge } from "../usage/Gauge";
import { formatAge } from "../usage/duration";
import {
  PROVIDER_LABEL,
  hasUsableNumbers,
  type ProviderAccount,
  type ProviderId,
  type ProviderSnapshot,
} from "../usage/types";

/** Each provider's own brand mark, kept in full colour — unlike the menu bar
 * icon, this sits inside a card that already says which provider it is in
 * words, so the mark is decoration rather than the only signal. */
const PROVIDER_ICON: Record<ProviderId, string> = {
  claude: claudeIcon,
  codex: codexIcon,
};

/**
 * Codex's mark is solid white — meant for a dark background, not a card
 * whose background flips between near-white and near-black with the system
 * theme. A fixed dark chip behind it means it stays visible either way,
 * instead of vanishing on the light theme.
 */
const PROVIDER_ICON_NEEDS_CHIP: Record<ProviderId, boolean> = {
  claude: false,
  codex: true,
};

type Props = {
  snapshot: ProviderSnapshot;
  now: number;
  /** Who is signed in and on what plan; absent until the first read. */
  account?: ProviderAccount;
};

/**
 * A reading younger than this counts as current and its age is not shown.
 *
 * Claude answers for the moment of the request, so its age is always a few
 * seconds and printing it would be noise. Codex's comes out of a log that may
 * not have been written to in days, and there the age is the most important
 * thing on the card.
 */
const CURRENT_MS = 60_000;

/**
 * One provider's card: a gauge per window, or an honest account of why there
 * is none.
 *
 * The window count is whatever the provider reports — Claude has two, this
 * Codex plan has one, a future plan may have three — so nothing here assumes a
 * fixed pair. They are ordered shortest window first, which is both stable and
 * the order that matters: the session limit bites long before the monthly one.
 */
export function ProviderCard({ snapshot, now, account }: Props) {
  const name = PROVIDER_LABEL[snapshot.provider];
  const showNumbers =
    hasUsableNumbers(snapshot.status) && snapshot.windows.length > 0;

  const age = now - snapshot.capturedAtMs;
  const showAge = showNumbers && age >= CURRENT_MS;

  const windows = [...snapshot.windows].sort(
    (a, b) => a.windowMinutes - b.windowMinutes,
  );

  // Numbers we can show but should not be trusted as current still get a line
  // saying what to do. Without it, `expired` would show dimmed gauges and no
  // explanation — visibly wrong, with no hint that running the CLI fixes it.
  const caveat = showNumbers ? caveatFor(snapshot) : null;

  return (
    <article className="card" data-status={snapshot.status}>
      <header className="card__head">
        <span className="card__title">
          <span
            className="card__icon-wrap"
            data-chip={PROVIDER_ICON_NEEDS_CHIP[snapshot.provider] || undefined}
          >
            <img
              className="card__icon"
              src={PROVIDER_ICON[snapshot.provider]}
              alt=""
            />
          </span>
          <span className="card__name">{name}</span>
          {account?.plan && <span className="card__plan">{account.plan}</span>}
        </span>
        {/* No "healthy" badge. Marking the normal case trains people to stop
            reading the marks, and then the one that matters goes unnoticed. */}
        {showAge && <span className="card__age">{formatAge(age)} 기준</span>}
      </header>

      {account?.email && <p className="card__account">{account.email}</p>}

      {showNumbers ? (
        <div className="card__gauges">
          {windows.map((w) => (
            <Gauge
              key={w.windowMinutes}
              window={w}
              now={now}
              muted={snapshot.status !== "ok"}
            />
          ))}
        </div>
      ) : (
        <p className="card__note">{noteFor(snapshot)}</p>
      )}

      {caveat && <p className="card__note card__note--caveat">{caveat}</p>}
    </article>
  );
}

/**
 * What to say when there are numbers but they are not current.
 *
 * `null` for a fresh reading: normal is unmarked.
 */
function caveatFor(snapshot: ProviderSnapshot): string | null {
  const name = PROVIDER_LABEL[snapshot.provider];
  switch (snapshot.status) {
    case "expired":
      return `${name}를 한 번 실행하면 다시 연결됩니다.`;
    case "stale":
      return "최신 값을 읽지 못했습니다. 계속 확인 중입니다.";
    default:
      return null;
  }
}

/**
 * What to say when there are no numbers at all.
 *
 * These cases look alike and mean entirely different things, so each gets its
 * own words — and none of them gets an error voice. An absent provider is an
 * invitation, not a deficiency: someone who uses one agent must not feel the
 * app is running at half capacity.
 */
function noteFor(snapshot: ProviderSnapshot): string {
  const name = PROVIDER_LABEL[snapshot.provider];

  if (snapshot.status === "notInstalled") {
    return `${name}를 쓰면 여기에 함께 표시됩니다.`;
  }
  if (snapshot.status === "expired") {
    return `${name}를 한 번 실행하면 다시 연결됩니다.`;
  }

  switch (snapshot.errorReason) {
    case "noPlan":
      // Installed, signed in, entitled to nothing. "Go run the CLI" would be
      // useless advice here, which is why this is its own case.
      return `${name} 계정에 표시할 한도가 없습니다.`;
    case "noData":
      return `아직 기록된 한도가 없습니다. ${name}를 한 번 실행하면 기록됩니다.`;
    case "windowReset":
      // The number we had described a period that has since ended. Showing it
      // dimmed would be a confident lie, so we show nothing and say why.
      return `마지막 기록이 만료됐습니다. ${name}를 한 번 실행하면 갱신됩니다.`;
    case "locked":
      // Not a retry problem. Saying "trying again shortly" would be a promise
      // the app cannot keep while nobody is at the machine.
      return "키체인이 잠겨 있습니다. 맥을 깨우고 접근을 허용해 주세요.";
    case "denied":
      // The prompt comes back after a backoff. Saying which button to press
      // then is the whole fix — "Allow" alone would only last until relaunch.
      return "키체인 접근이 거부됐어요. 잠시 후 다시 물어보면 '항상 허용'을 눌러 주세요.";
    case "readFailed":
      return "한도를 읽지 못했습니다. 잠시 후 다시 확인합니다.";
    default:
      return "한도를 알 수 없습니다.";
  }
}
