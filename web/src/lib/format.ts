// Small display helpers shared by the pages. Times from the API are Unix seconds.

import { date, dateTime } from './date'

export function minutes(secs: number | null): string {
  return secs === null ? 'unknown' : `${Math.round(secs / 60)} min`
}

export function when(unix: number | null): string {
  if (unix === null) return 'never'
  return dateTime(unix)
}

/** `YYYYMMDD` from yt-dlp as `YYYY-MM-DD`. */
export function uploadDate(published: number | null, ytdlp: string | null): string {
  if (published !== null) return date(published)
  if (ytdlp && ytdlp.length === 8) return `${ytdlp.slice(0, 4)}-${ytdlp.slice(4, 6)}-${ytdlp.slice(6)}`
  return 'unknown'
}

export function seconds(secs: number): string {
  if (secs < 90) return `${Math.round(secs)} s`
  if (secs < 5400) return `${Math.round(secs / 60)} min`
  return `${Math.round(secs / 3600)} h`
}

export function youtube(videoId: string): string {
  return `https://www.youtube.com/watch?v=${encodeURIComponent(videoId)}`
}

/** A config interval in the largest whole unit: 900 is "15 min", 4800 "80 min", 86400 "24 h". */
export function duration(secs: number): string {
  if (secs >= 3600 && secs % 3600 === 0) return `${secs / 3600} h`
  if (secs >= 60 && secs % 60 === 0) return `${secs / 60} min`
  return `${secs} s`
}

/** A byte count in the largest binary unit that keeps it at 1 or more: 1536 is "1.5 KiB". */
export function bytes(n: number): string {
  const units = ['bytes', 'KiB', 'MiB', 'GiB', 'TiB']
  let i = 0
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024
    i++
  }
  return i === 0 ? `${n} bytes` : `${n.toFixed(1)} ${units[i]}`
}
