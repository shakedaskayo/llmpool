import { expect, mock, test } from 'claude-code/testing'

// What `llmpool status --json` answers for a session on work-1 at 91%, with
// one account to switch to, one disabled and one that needs a login.
const STATUS = {
  session: { id: 's1', account_id: 'a', picked_by: 'rule "work repos"', advice: { switch: 0.82, target: 'work-2', acted: false } },
  config: { prefer_own: true, advisor_enabled: true, has_key: true },
  accounts: [
    { id: 'a', label: 'work-1', tags: ['work'], enabled: true, health: 'ok', sessions: ['s1'],
      // The clock in these tests reads 1_700_000_000: the 5-hour window is half over.
      history: [[1, 20, 5], [2, 55, 8], [3, 91, 12]],
      usage: { windows: { '5h': { used_percent: 91, resets_at: 1_700_009_000 }, '7d': { used_percent: 12, resets_at: null } } } },
    { id: 'b', label: 'work-2', tags: ['work'], enabled: true, health: 'ok', sessions: [], usage: null },
    { id: 'c', label: 'spare', tags: [], enabled: false, health: 'ok', sessions: [], usage: null },
    { id: 'd', label: 'old', tags: [], enabled: true, health: 'relogin_required', sessions: [], usage: null },
    { id: 'e', label: 'busy', tags: [], enabled: true, health: 'ok', sessions: ['s2'], usage: null },
  ],
  sessions: [{ id: 's1', account_label: 'work-1', cwd: '/w' }],
  issues: [{ kind: 'relogin', text: 'old needs a new sign-in.', action: 'login', email: 'old@x.io' }],
  login: { state: 'idle' },
  stats: { headroom_5h: 70, headroom_7d: 96, switches_24h: 3, last_switch: { at: 1_700_000_000, text: 'this session moved from work-2 to work-1 (rule "swap")' } },
}

// The test stands in for the host: these are the calls the mod makes that a
// real session answers itself.
const host = (on: any) => {
  on('store.get', () => ({ value: undefined }))
  on('store.set', () => ({ value: undefined }))
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
    for (const text of [/work-1/, /this session/, /work-2/, /near limit/, /91%/, /disabled/, /needs a new sign-in/]) {
      expect(await ui.find({ type: 'Text', text })).toBeDefined()
    }
    // The current account is drawn large: a meter with the pace mark, a
    // forecast, and its trend. Half the window gone, 91% used: it will not last.
    expect(await ui.find({ type: 'Text', text: /┃/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /at this pace full in 1[45]m/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /▂▅█/ })).toBeDefined()
    // Why this session is on this account.
    expect(await ui.find({ type: 'Text', text: /rule "work repos"/ })).toBeDefined()
    // What the advisor last said, and the switches that can be thrown from here.
    expect(await ui.find({ type: 'Text', text: /advisor: 82% for switching to work-2/ })).toBeDefined()
    expect(await ui.find({ key: 'set-own' })).toBeDefined()
    await ui.press({ key: 'set-advisor' })
    expect(calls.some(argv => argv.slice(1).join(' ') === 'advisor off')).toBe(true)
    await ui.press({ key: 'ask-advisor' })
    expect(calls.some(argv => argv.slice(1).join(' ') === 'advisor ask --session s1')).toBe(true)
    // The pool as a whole.
    expect(await ui.find({ type: 'Text', text: /70% free/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /3 switches in the last 24h/ })).toBeDefined()
    // Only the usable, other account can be switched to.
    expect(await ui.find({ key: 'use-b' })).toBeDefined()
    expect(await ui.find({ key: 'use-a' })).toBeUndefined()
    expect(await ui.find({ key: 'use-c' })).toBeUndefined()
    expect(await ui.find({ key: 'use-d' })).toBeUndefined()
    // Nor one whose login is live in another session.
    expect(await ui.find({ key: 'use-e' })).toBeUndefined()
    expect(await ui.find({ type: 'Text', text: /in use by another session/ })).toBeDefined()

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
    // The way to the dashboard is on the row too.
    expect(await ui.find({ key: 'band-open' })).toBeDefined()
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

test('what is missing sits at the top with the press that fixes it; signing in starts from the pane', async ($, on) => {
  host(on)
  const calls: string[][] = []
  let status: Record<string, unknown> = STATUS
  on('process.run', (_$, e) => {
    calls.push([...e.argv].slice(1))
    if (e.argv[1] === 'add' && e.argv.includes('--detach')) {
      status = { ...STATUS, login: { state: 'waiting', message: 'Finish signing in in your browser.', url: 'https://example/login' } }
    }
    if (e.argv[1] === 'add' && e.argv.includes('--cancel')) {
      status = STATUS
    }

    return { value: { exitCode: 0, stdout: e.argv[1] === 'status' ? JSON.stringify(status) : 'ok', stderr: '' } } as never
  })
  await $.command.run({ command: 'llmpool', args: '' } as never)
  const ui = await $.ui.mount({ plugin: 'llmpool', surface: 'terminal', component: 'Pane', requestId: 'llmpool', props: { title: 'llmpool' } as never })
  expect(await ui.find({ type: 'Text', text: /old needs a new sign-in/ })).toBeDefined()

  // Fixing the dead login signs in as that account, in the browser.
  await ui.press({ key: 'fix-0' })
  expect(calls).toContainEqual(['add', 'claude', '--detach', '--email', 'old@x.io'])
  await ui.redraw()
  expect(await ui.find({ type: 'Text', text: /Waiting for you to finish signing in/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /https:\/\/example\/login/ })).toBeDefined()
  // A page that shows a code instead of returning by itself: paste it here.
  await ui.input({ key: 'login-code', text: ' c0de#st4te ' })
  expect(calls).toContainEqual(['add', 'claude', '--detach', '--code', 'c0de#st4te'])
  // While one sign-in waits, a second cannot be started; it can be cancelled.
  expect(await ui.find({ key: 'add' })).toBeUndefined()
  await ui.press({ key: 'login-cancel' })
  expect(calls).toContainEqual(['add', 'claude', '--cancel'])
  await ui.redraw()

  // Adding a brand-new account is one key in the actions row.
  await ui.press({ key: 'add' })
  expect(calls.filter(argv => argv.join(' ') === 'add claude --detach').length).toBe(1)
  await ui.unmount()
})

test('a login that is not in the pool is offered from the band, in one press', async ($, on) => {
  host(on)
  const calls: string[][] = []
  const status = {
    session: null,
    accounts: [],
    sessions: [],
    issues: [{ kind: 'login_not_in_pool', text: 'The login Claude Code uses now (me@x.io) is not in the pool, so llmpool cannot switch it.', action: 'add_current', email: 'me@x.io' }],
    login: { state: 'idle' },
  }
  on('process.run', (_$, e) => {
    calls.push([...e.argv].slice(1))

    return { value: { exitCode: 0, stdout: e.argv[1] === 'status' ? JSON.stringify(status) : 'added me@x.io', stderr: '' } } as never
  })
  await $.command.run({ command: 'llmpool', args: '' } as never)
  const ui = await $.ui.mount({ plugin: 'llmpool', surface: 'terminal', component: 'AbovePrompt', props: {} as never })
  expect(await ui.find({ type: 'Text', text: /is not in the pool/ })).toBeDefined()
  await ui.press({ key: 'band-fix' })
  expect(calls).toContainEqual(['add', 'claude', '--from-current'])
  await ui.unmount()
})

test('the way in is a tail on the hint line, and nothing is pinned twice', async ($, on) => {
  const pinned: unknown[] = []
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  on('ui.status', (_$, e) => {
    pinned.push(e)

    return { value: undefined } as never
  })
  on('session.id', () => ({ value: 'agent-session-1' }) as never)
  on('clock.now', () => ({ value: 1_700_000_000_000 }) as never)
  on('process.run', (_$, e) => ({ value: { exitCode: 0, stdout: e.argv[1] === 'status' ? JSON.stringify(STATUS) : '', stderr: '' } }) as never)
  await $.command.run({ command: 'llmpool', args: '' } as never)
  // The status line already shows the account; the mod must not pin a copy.
  expect(pinned).toEqual([])
})

test('with room to spare the row above the prompt is a quiet quick switch, and it can be turned off', async ($, on) => {
  host(on)
  const calm = { ...STATUS, accounts: STATUS.accounts.map(a => (a.id === 'a' ? { ...a, usage: { windows: { '5h': { used_percent: 20, resets_at: 1_700_009_000 } } } } : a)) }
  const calls: string[][] = []
  on('process.run', (_$, e) => {
    calls.push([...e.argv].slice(1))

    return { value: { exitCode: 0, stdout: e.argv[1] === 'status' ? JSON.stringify(calm) : 'ok', stderr: '' } } as never
  })
  await $.command.run({ command: 'llmpool', args: '' } as never)
  const ui = await $.ui.mount({ plugin: 'llmpool', surface: 'terminal', component: 'AbovePrompt', props: {} as never })
  // No warning, just the accounts one press away.
  expect(await ui.find({ type: 'Text', text: /limit/ })).toBeUndefined()
  expect(await ui.find({ type: 'Text', text: /switch to/ })).toBeDefined()
  await ui.press({ key: 'band-switch' })
  expect(calls).toContainEqual(['switch', 'b', '--session', 's1'])

  const said = await $.command.run({ command: 'llmpool', args: 'row off' } as never)
  expect(JSON.stringify(said)).toContain('row above the prompt is off')
  // Turned off, the mod draws nothing there and hands the row back to the
  // engine; in this kit nothing stands beneath it, which is how that shows.
  let handedBack = ''
  try {
    await ui.redraw()
  } catch (error) {
    handedBack = String(error)
  }
  expect(handedBack).toContain('no implementation for ui.render')
})

// An environment the hooks read and write, as a session's process holds it.
const gatewayEnv = (on: any, answer: string) => {
  const env: Record<string, string | undefined> = { ANTHROPIC_AUTH_TOKEN: 't', ANTHROPIC_BASE_URL: 'https://pool.example/gateway/claude' }
  const seen: Array<Record<string, string | undefined>> = []
  on('env.get', (_$: any, e: any) => ({ value: env[e.name] }))
  on('env.set', (_$: any, e: any) => {
    env[e.name] = e.value

    return { value: undefined }
  })
  on('tool.call', { tool: 'AskUserQuestion' }, (_$: any, e: any) => ({
    result: { questions: e.questions, answers: { [e.questions[0].question]: answer } },
  }))
  on('tool.call', { tool: 'Artifact' }, () => {
    seen.push({ ...env })

    return { result: { text: 'published' } } as never
  })

  return { env, seen }
}

test('on the gateway, an Artifact runs on the own login once the person says yes, then the session is back on the pool', async ($, on) => {
  host(on)
  const { env, seen } = gatewayEnv(on, 'Yes, this time')
  await $.tool.call({ tool: 'Artifact', file_path: 'a.html' } as never)
  expect(seen).toEqual([{ ANTHROPIC_AUTH_TOKEN: undefined, ANTHROPIC_API_KEY: undefined, ANTHROPIC_BASE_URL: undefined }])
  expect(env.ANTHROPIC_AUTH_TOKEN).toBe('t')
  expect(env.ANTHROPIC_BASE_URL).toBe('https://pool.example/gateway/claude')
})

test('on the gateway, a no keeps the session on the pool and the Artifact does not run', async ($, on) => {
  host(on)
  const { env, seen } = gatewayEnv(on, 'Not now')
  const ran: any = await $.tool.call({ tool: 'Artifact', file_path: 'a.html' } as never).catch((err: Error) => ({ deny: err.message }))
  expect(JSON.stringify(ran)).toMatch(/local file/)
  expect(seen).toEqual([])
  expect(env.ANTHROPIC_AUTH_TOKEN).toBe('t')
})
