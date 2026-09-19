import { formatAge } from "../usage/duration";

type Props = {
  lastCheckedAtMs: number | null;
  checkIntervalMs: number;
  now: number;
};

/**
 * How far past the expected interval a check can be before we call it stopped.
 *
 * Generous on purpose: the check can be late for boring reasons — the machine
 * slept, a request took its full timeout — and an indicator that cries wolf on
 * ordinary jitter is worse than none.
 */
const OVERDUE_FACTOR = 3;

/**
 * A heartbeat, in the shape a status page uses: a dot and a few words.
 *
 * There used to be a refresh button here, and its real job was reassurance —
 * press it, watch it respond, know the app is alive. The dot alone replaced
 * the pulse but not the sentence, and a dot with no label asks people to
 * remember what green means. So the words came back, compact and beside it,
 * rather than as a line of their own at the bottom.
 *
 * The dot still pulses once per check, which is the part a static label cannot
 * do: it shows the app working, not merely claiming to have worked.
 */
export function HealthDot({ lastCheckedAtMs, checkIntervalMs, now }: Props) {
  const age = lastCheckedAtMs === null ? null : Math.max(0, now - lastCheckedAtMs);
  const overdue = age !== null && age > checkIntervalMs * OVERDUE_FACTOR;

  // Three states, and the middle one matters: before the first check we know
  // nothing yet, which is neither alive nor stopped.
  const state = age === null ? "waiting" : overdue ? "overdue" : "ok";

  const label =
    age === null
      ? "확인 중"
      : overdue
        ? `${formatAge(age)} 확인 안 됨`
        : `${formatAge(age)} 갱신됨`;

  const cadence = Math.round(checkIntervalMs / 1000);
  const cadenceText =
    cadence >= 60 ? `${Math.round(cadence / 60)}분마다` : `${cadence}초마다`;

  return (
    <span
      className="health"
      data-state={state}
      // The cadence is a detail, not a headline: on hover for whoever wants it.
      title={`${cadenceText} 자동 확인`}
      role="status"
    >
      <span className="health__mark" aria-hidden="true">
        {/* Keyed on the check time so React remounts it, which is what
            replays a one-shot CSS animation. Without the key the ring would
            expand once and never again. */}
        {!overdue && lastCheckedAtMs !== null && (
          <span key={lastCheckedAtMs} className="health__pulse" />
        )}
        <span className="health__dot" />
      </span>
      <span className="health__label">{label}</span>
    </span>
  );
}
