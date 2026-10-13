import { expect, test } from 'claude-code/testing'

import { WINDOW_SECS, adviceLine, bar, best, elapsed, fit, forecast, fullest, level, miniBar, peak, resetsIn, spark, stateMark, toSnapshot, wideBar } from './view'

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
  expect(snap.issues).toEqual([])
  expect(snap.login.state).toBe('idle')
  expect(snap.others).toEqual([{ id: 's2', label: 'default login', cwd: '/x' }])
  expect(peak(snap.rows[0]!)).toBe(91.4)
})

test('a session llmpool did not start has no current account', () => {
  const snap = toSnapshot({ ...STATUS, session: null })
  expect(snap.session).toBe(null)
  expect(snap.rows.some(r => r.isCurrent)).toBe(false)
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

test('a wide meter is smooth and carries the pace mark', () => {
  const text = (percent: number | null, pace: number | null) => wideBar(percent, 10, pace).map(p => p.text).join('')
  expect(text(0, null)).toBe('░░░░░░░░░░')
  expect(text(50, null)).toBe('█████░░░░░')
  expect(text(55, null)).toBe('█████▌░░░░')
  expect(text(100, null)).toBe('██████████')
  expect(text(250, null)).toBe('██████████')
  // The mark sits where even use would be: behind it is under pace, past it over.
  expect(text(20, 0.5)).toBe('██░░░┃░░░░')
  expect(text(80, 0.5)).toBe('█████┃██░░')
  expect(wideBar(80, 10, 0.5).map(p => p.kind)).toEqual(['used', 'pace', 'used', 'free'])
  for (const [percent, pace] of [[0, 0], [33, 1], [100, 0.99], [7, 0.07]] as const) {
    expect([...text(percent, pace)].length).toBe(10)
  }
  expect(text(null, 0.5)).toBe('░░░░░░░░░░')
})

test('pace, forecast and trend', () => {
  const now = 1_000_000_000
  const five = WINDOW_SECS['5h']
  // Half the window gone.
  const reset = now / 1000 + five / 2
  expect(elapsed(reset, now, five)).toBe(0.5)
  expect(elapsed(null, now, five)).toBe(null)
  expect(forecast(25, reset, now, five)).toBe('at this pace it lasts to the reset')
  expect(forecast(80, reset, now, five)).toBe('at this pace full in 38m, 1h 53m before it resets')
  expect(forecast(100, reset, now, five)).toBe('limit reached')
  // Too early in the window, or nothing used yet, to say anything.
  expect(forecast(50, now / 1000 + five - 60, now, five)).toBe('')
  expect(forecast(1, reset, now, five)).toBe('')
  expect(forecast(null, reset, now, five)).toBe('')

  expect(spark([0, 50, 100])).toBe('▁▅█')
  expect(spark([42])).toBe('')
  expect(miniBar(null)).toBe('▱▱▱▱▱')
  expect(miniBar(42)).toBe('▰▰▱▱▱')
  expect(fit('abc', 6)).toBe('abc   ')
  expect(fit('abcdefgh', 6)).toBe('abcde…')
})

test('the advisor is quoted in one line', () => {
  expect(adviceLine(null)).toBe('')
  expect(adviceLine({ switch: null })).toBe('advisor: asking…')
  expect(adviceLine({ switch: 0.12, target: null, acted: false })).toBe('advisor: 12% for switching')
  expect(adviceLine({ switch: 0.9, target: 'work-2', acted: true })).toBe('advisor: 90% for switching to work-2 · switched')
  expect(adviceLine({ error: 'the advisor account has no tokens left' })).toBe('advisor: the advisor account has no tokens left')
})

test('keys and other agents\' logins are not offered to a Claude Code session', () => {
  const snap = toSnapshot({
    ...STATUS,
    accounts: [
      ...STATUS.accounts,
      { id: 'k', label: 'openai-work', provider: 'openai', kind: 'api_key', tags: [], enabled: true, health: 'ok', sessions: [] },
      { id: 'x', label: 'codex:me', provider: 'codex', kind: 'oauth', tags: [], enabled: true, health: 'ok', sessions: [] },
      { id: 'g', label: 'team gateway (claude)', provider: 'claude', kind: 'api_key', tags: [], enabled: true, health: 'ok', sessions: [] },
      { id: 'd', label: 'work-3', provider: 'claude', kind: 'oauth', tags: [], enabled: true, health: 'ok', sessions: [] },
    ],
  })
  expect(snap.rows.map(r => r.label).sort()).toEqual(['old', 'work-1', 'work-2', 'work-3'])
})
