import { windowLabel, type LimitWindow } from "./types";
import { formatRemaining } from "./duration";

type Props = {
  window: LimitWindow;
  now: number;
  /** Dim the gauge when the numbers are not current. */
  muted: boolean;
};

/**
 * One window: name, how much is left, bar, and time to reset.
 *
 * The bar fills with what **remains**, so a draining gauge empties rather than
 * filling up. Mixing that with a usage bar elsewhere would put two opposite
 * meanings on the same shape.
 */
export function Gauge({ window: limit, now, muted }: Props) {
  const remaining = Math.round(limit.remainingPercent);
  const untilReset = limit.resetsAtMs - now;

  return (
    <section className="gauge" data-muted={muted || undefined}>
      <div className="gauge__head">
        <span className="gauge__label">{windowLabel(limit.windowMinutes)}</span>
        <span className="gauge__percent">{remaining}% 남음</span>
      </div>

      <div
        className="gauge__track"
        role="meter"
        aria-valuenow={remaining}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label={`${windowLabel(limit.windowMinutes)} 잔량`}
      >
        <div
          className="gauge__fill"
          style={{ width: `${limit.remainingPercent}%` }}
        />
      </div>

      <p className="gauge__reset">{formatRemaining(untilReset)} 후 리셋</p>
    </section>
  );
}
