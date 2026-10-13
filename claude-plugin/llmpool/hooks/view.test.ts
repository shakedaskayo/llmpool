import { expect, test } from 'claude-code/testing'

import { bar, best, fullest, level, peak, resetsIn, stateMark, statusText, toSnapshot } from './view'

const STATUS = {
  session: { id: 's1', account_id: 'a' },
  accounts: [
    {
      id: 'a',
      label: 'work-1',
      tags: ['work'],
      enabled: true,
      health: 'ok',
      sessions: ['s1'],
      usage: { windows: { '5h': { used_percent: 91.4, resets_at: 7200 }, '7d': { used_percent: 12, resets_at: null } } },
    },
    { id: 'b', label: 'work-2', tags: [], enabled: false, health: 'ok', sessions: [], usage: null },
    { id: 'c', label: 'old', tags: [], enabled: true, health: 'relogin_required', sessions: [], usage: null },
  ],
  sessions: [
    { id: 's1', account_label: 'work-1', cwd: '/w' },
    { id: 's2', account_label: null, cwd: '/x' },
  ],
}

test('status json becomes rows, with the current account marked', () => {
  const snap = toSnapshot(STATUS)
  expect(snap.session).toBe('s1')
  expect(snap.rows.map(r => [r.label, r.isCurrent, r.state])).toEqual([
    ['work-1', true, 'ok'],
    ['old', false, 'relogin required'],
    ['work-2', false, 'disabled'],
  ])
  expect(snap.rows[0]!.h5).toBe(91.4)
  expect(snap.rows[1]!.h5).toBe(null)
  expect(snap.rows.map(stateMark).map(m => m.icon)).toEqual(['●', '⚠', '⏸'])
  expect(snap.others).toEqual([{ id: 's2', label: 'default login', cwd: '/x' }])
  expect(statusText(snap)).toBe('llmpool: work-1 · 5h 91% · 7d 12%')
  expect(peak(snap.rows[0]!)).toBe(91.4)
})

test('a session llmpool did not start has no current account', () => {
  const snap = toSnapshot({ ...STATUS, session: null })
  expect(snap.session).toBe(null)
  expect(snap.rows.some(r => r.isCurrent)).toBe(false)
  expect(statusText(snap)).toBe(undefined)
})

test('accounts are ordered: current, then switchable with the most room, then the rest', () => {
  const account = (id: string, percent: number | null, extra = {}) => ({
    id,
    label: id,
    tags: [],
    enabled: true,
    health: 'ok',
    sessions: [],
    usage: percent === null ? null : { windows: { '5h': { used_percent: percent, resets_at: null } } },
    ...extra,
  })
  const snap = toSnapshot({
    session: { id: 's1', account_id: 'busy' },
    accounts: [account('dead', 0, { health: 'relogin_required' }), account('half', 50), account('busy', 95), account('fresh', null)],
    sessions: [],
  })
  expect(snap.rows.map(r => r.id)).toEqual(['busy', 'fresh', 'half', 'dead'])
  expect(best(snap)?.id).toBe('fresh')
  expect(fullest(snap.rows[0]!)).toEqual({ name: '5-hour', percent: 95, resetAt: null })
  // No account is meaningfully better than a nearly empty one.
  const calm = toSnapshot({ session: { id: 's1', account_id: 'fresh' }, accounts: [account('fresh', 5), account('half', 3)], sessions: [] })
  expect(best(calm)).toBe(undefined)
})

test('fullness is said in a word as well as a colour', () => {
  expect(level(null)).toEqual({ color: 'inactive', word: 'no reading yet' })
  expect(level(10).word).toBe('plenty left')
  expect(level(70)).toEqual({ color: 'warning', word: 'filling up' })
  expect(level(90)).toEqual({ color: 'error', word: 'near limit' })
  expect(level(100).word).toBe('limit reached')
})

test('meter and reset text', () => {
  expect(bar(null)).toBe('▱▱▱▱▱▱▱▱▱▱')
  expect(bar(0)).toBe('▱▱▱▱▱▱▱▱▱▱')
  expect(bar(42)).toBe('▰▰▰▰▱▱▱▱▱▱')
  expect(bar(250)).toBe('▰▰▰▰▰▰▰▰▰▰')
  expect(resetsIn(7200, 0)).toBe('resets in 2h 0m')
  expect(resetsIn(90000, 0)).toBe('resets in 1d 1h')
  expect(resetsIn(300, 0)).toBe('resets in 5m')
  expect(resetsIn(100, 200000)).toBe('')
  expect(resetsIn(null, 0)).toBe('')
})
