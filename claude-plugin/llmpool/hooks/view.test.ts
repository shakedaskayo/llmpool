import { expect, test } from 'claude-code/testing'

import { bar, peak, resetsIn, statusText, toSnapshot } from './view'

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
  const snap = toSnapshot(STATUS, 's1')
  expect(snap.session).toBe('s1')
  expect(snap.rows.map(r => [r.label, r.isCurrent, r.state])).toEqual([
    ['work-1', true, 'ok'],
    ['work-2', false, 'disabled'],
    ['old', false, 'relogin required'],
  ])
  expect(snap.rows[0].h5).toBe(91.4)
  expect(snap.rows[1].h5).toBe(null)
  expect(snap.others).toEqual([{ id: 's2', label: 'default login', cwd: '/x' }])
  expect(statusText(snap)).toBe('llmpool: work-1 · 5h 91% · 7d 12%')
  expect(peak(snap.rows[0])).toBe(91.4)
})

test('a session llmpool did not start has no current account', () => {
  const snap = toSnapshot({ ...STATUS, session: null }, null)
  expect(snap.session).toBe(null)
  expect(snap.rows.some(r => r.isCurrent)).toBe(false)
  expect(statusText(snap)).toBe(undefined)
})

test('meter and reset text', () => {
  expect(bar(null)).toBe('··········')
  expect(bar(0)).toBe('░░░░░░░░░░')
  expect(bar(42)).toBe('████░░░░░░')
  expect(bar(250)).toBe('██████████')
  expect(resetsIn(7200, 0)).toBe('resets in 2h 0m')
  expect(resetsIn(90000, 0)).toBe('resets in 1d 1h')
  expect(resetsIn(300, 0)).toBe('resets in 5m')
  expect(resetsIn(100, 200000)).toBe('')
  expect(resetsIn(null, 0)).toBe('')
})
