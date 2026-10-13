import { expect, test } from 'claude-code/testing'

// What `llmpool status --json` answers for a session on work-1 at 91%, with
// one account to switch to, one disabled and one that needs a login.
const STATUS = {
  session: { id: 's1', account_id: 'a' },
  accounts: [
    { id: 'a', label: 'work-1', tags: ['work'], enabled: true, health: 'ok', sessions: ['s1'],
      usage: { windows: { '5h': { used_percent: 91, resets_at: 4000000000 }, '7d': { used_percent: 12, resets_at: null } } } },
    { id: 'b', label: 'work-2', tags: ['work'], enabled: true, health: 'ok', sessions: [], usage: null },
    { id: 'c', label: 'spare', tags: [], enabled: false, health: 'ok', sessions: [], usage: null },
    { id: 'd', label: 'old', tags: [], enabled: true, health: 'relogin_required', sessions: [], usage: null },
  ],
  sessions: [{ id: 's1', account_label: 'work-1', cwd: '/w' }],
}

// The test stands in for the host: these are the calls the mod makes that a
// real session answers itself.
const host = (on: any) => {
  on('ui.open', () => ({ value: { isPlaced: true } }))
  on('ui.status', () => ({ value: undefined }))
  on('ui.toast', () => ({ value: undefined }))
  on('session.id', () => ({ value: 'agent-session-1' }))
  on('clock.now', () => ({ value: 1_700_000_000_000 }))
}

for (const surface of ['terminal', 'desktop'] as const) {
  test(`the pane shows every account and switches on a press (${surface})`, async ($, on) => {
    host(on)
    const calls: string[][] = []
    // Stands in for the llmpool binary.
    on('process.run', (_$, e) => {
      calls.push([...e.argv])
      const isStatus = e.argv[1] === 'status'

      return { value: { exitCode: 0, stdout: isStatus ? JSON.stringify(STATUS) : 'session s1 now uses work-2', stderr: '' } } as never
    })

    await $.command.run({ command: 'llmpool', args: '' } as never)
    const ui = await $.ui.mount({ plugin: 'llmpool', surface, component: 'Pane', requestId: 'llmpool', props: { title: 'llmpool' } as never })

    // Every account is there, each with its state said in words.
    for (const text of [/work-1/, /this session/, /work-2/, /near limit/, /91%/, /disabled/, /needs a new login/]) {
      expect(await ui.find({ type: 'Text', text })).toBeDefined()
    }
    // Only the usable, other account can be switched to.
    expect(await ui.find({ key: 'use-b' })).toBeDefined()
    expect(await ui.find({ key: 'use-a' })).toBeUndefined()
    expect(await ui.find({ key: 'use-c' })).toBeUndefined()
    expect(await ui.find({ key: 'use-d' })).toBeUndefined()

    await ui.press({ key: 'use-b' })
    const switched = calls.find(argv => argv[1] === 'switch')
    expect(switched?.slice(1)).toEqual(['switch', 'b', '--session', 's1'])
    await ui.unmount()
  })

  test(`the band appears near the limit with a one-press switch (${surface})`, async ($, on) => {
    host(on)
    const calls: string[][] = []
    on('process.run', (_$, e) => {
      calls.push([...e.argv])

      return { value: { exitCode: 0, stdout: e.argv[1] === 'status' ? JSON.stringify(STATUS) : '', stderr: '' } } as never
    })
    await $.command.run({ command: 'llmpool', args: '' } as never)
    const ui = await $.ui.mount({ plugin: 'llmpool', surface, component: 'AbovePrompt', props: {} as never })
    expect(await ui.find({ type: 'Text', text: /91% of the 5-hour limit/ })).toBeDefined()
    await ui.press({ key: 'band-switch' })
    expect(calls.some(argv => argv[1] === 'switch' && argv[2] === 'b')).toBe(true)
    await ui.unmount()
  })
}

test('with llmpool not answering, the pane says so and how to retry', async ($, on) => {
  host(on)
  on('process.run', () => ({ value: { exitCode: 1, stdout: '', stderr: 'llmpool: daemon unreachable' } }) as never)
  await $.command.run({ command: 'llmpool', args: '' } as never)
  const ui = await $.ui.mount({ plugin: 'llmpool', surface: 'terminal', component: 'Pane', requestId: 'llmpool', props: { title: 'llmpool' } as never })
  expect(await ui.find({ type: 'Text', text: /not answering/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /daemon unreachable/ })).toBeDefined()
  expect(await ui.find({ key: 'refresh' })).toBeDefined()
  await ui.unmount()
})

test('`/llmpool next` switches without opening anything', async ($, on) => {
  host(on)
  const calls: string[][] = []
  on('process.run', (_$, e) => {
    calls.push([...e.argv])

    return { value: { exitCode: 0, stdout: e.argv[1] === 'status' ? JSON.stringify(STATUS) : 'session s1 now uses work-2', stderr: '' } } as never
  })
  const ran = await $.command.run({ command: 'llmpool', args: 'next' } as never)
  expect(calls.find(argv => argv[1] === 'switch')?.slice(1)).toEqual(['switch', '--next', '--session', 's1'])
  expect(JSON.stringify(ran)).toContain('now uses work-2')
})
