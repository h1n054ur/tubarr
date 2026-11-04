// The one date format for every page: `2026-10-23 14:31` (24 h, local time) and `2026-10-23`.
// Times from the API are Unix seconds.

const pad = (n: number) => String(n).padStart(2, '0')

export function date(unix: number): string {
  const d = new Date(unix * 1000)
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

export function dateTime(unix: number): string {
  const d = new Date(unix * 1000)
  return `${date(unix)} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}
