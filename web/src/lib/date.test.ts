import { describe, expect, it } from 'vitest'
import { date, dateTime } from './date'

// Local time, so the inputs are built from a local Date rather than a fixed zone.
const unix = (y: number, mo: number, d: number, h = 0, mi = 0, s = 0) =>
  new Date(y, mo - 1, d, h, mi, s).getTime() / 1000

describe('date', () => {
  it('writes the local calendar day with zero padding', () => {
    expect(date(unix(2026, 1, 5, 23, 59))).toBe('2026-01-05')
    expect(date(unix(2026, 10, 23, 0, 0))).toBe('2026-10-23')
  })
})

describe('dateTime', () => {
  it('writes 24 hour local time to the minute', () => {
    expect(dateTime(unix(2026, 10, 23, 14, 31, 19))).toBe('2026-10-23 14:31')
    expect(dateTime(unix(2026, 3, 4, 0, 5))).toBe('2026-03-04 00:05')
    expect(dateTime(unix(2026, 12, 31, 23, 59, 59))).toBe('2026-12-31 23:59')
  })
})
