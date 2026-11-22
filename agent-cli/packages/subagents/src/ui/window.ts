/**
 * Window a list around its selection, so a 200-agent fan-out still shows the
 * row you are on. Mirrors `fleet-list.ts`'s arithmetic.
 */
export function windowRange(selected: number, total: number, max: number): { start: number; end: number } {
  const visible = Math.min(max, total);
  const start = selected < visible ? 0 : selected - visible + 1;
  return { start, end: start + visible };
}
