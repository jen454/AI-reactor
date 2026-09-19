/**
 * Human durations for the countdowns.
 *
 * Mirrors `usage::format::format_remaining` on the Rust side. Deliberately
 * coarse: the largest two units and nothing finer. A reset four days away does
 * not need seconds, and a ticking seconds digit in a menu bar panel draws the
 * eye to a number nobody is waiting on.
 */
export function formatRemaining(ms: number): string {
  if (ms <= 0) return "지금";

  const minutes = Math.floor(ms / 60_000);
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  const mins = minutes % 60;

  if (days > 0) return `${days}일 ${hours}시간`;
  if (hours > 0) return `${hours}시간 ${mins}분`;
  return `${mins}분`;
}

/** "방금", "3분 전", "2시간 전" — how old a snapshot is. */
export function formatAge(ms: number): string {
  if (ms < 60_000) return "방금";
  return `${formatRemaining(ms)} 전`;
}
