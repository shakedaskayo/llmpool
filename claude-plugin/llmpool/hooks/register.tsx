import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { Issue, Row, Snapshot } from '../types'
import {
  bar,
  best,
  fixLabel,
  fullest,
  isUsable,
  level,
  peak,
  percentText,
  resetsIn,
  stateMark,
  toSnapshot,
  urgent,
} from './view'

// `llmpool install claude` replaces this with the installed binary's path.
const INSTALLED = '__LLMPOOL_BIN__'
const BIN = INSTALLED.startsWith('__') ? 'llmpool' : INSTALLED

const PANE = 'llmpool'
const TITLE = 'llmpool · accounts'
const REFRESH_MS = 15000
/** While a sign-in waits in the browser, look more often. */
const WAITING_MS = 2500
/** Usage at which the band above the prompt appears. */
const WARN_AT = 80

const EMPTY: Snapshot = {
  session: null,
  rows: [],
  others: [],
  error: null,
  issues: [],
  login: { state: 'idle', message: '', url: null },
}
const snapshot = atom({ plugin: 'llmpool', key: 'snapshot' } as const, EMPTY)

// What the last look at a sign-in said, so its ending is announced once.
let lastLogin = 'idle'

// Every argument is made a string here: the host refuses anything else, and
// one stray `undefined` would take the whole pane down with it.
const llmpool = ($: EngineInterface, args: unknown[]) =>
  $.process.run([BIN, ...args.map(arg => String(arg ?? ''))], { timeoutMs: 8000 })

const refresh = async ($: EngineInterface) => {
  let next: Snapshot
  try {
    // Claude Code's own id for this session is how llmpool finds a session it
    // did not launch. Asked each time: it changes on /clear and on resume.
    const ran = await llmpool($, ['status', '--json', '--agent-session', await $.session.id()])
    next =
      ran.exitCode === 0
        ? toSnapshot(JSON.parse(ran.stdout))
        : { ...EMPTY, error: ran.stderr.trim() || 'llmpool status failed' }
  } catch (error) {
    next = { ...EMPTY, error: `could not run ${BIN}: ${String(error)}` }
  }
  await update($, snapshot, () => next)
  // The account and its usage are on the status line already; nothing is
  // pinned here, or the same line would show twice. A sign-in ending is news.
  if (lastLogin === 'waiting' && next.login.state !== 'waiting' && next.login.message !== '') {
    $.ui.toast(next.login.message)
  }
  lastLogin = next.login.state

  return next
}

const say = ($: EngineInterface, ran: { exitCode: number; stdout: string; stderr: string }, ok: string) =>
  $.ui.toast(ran.exitCode === 0 ? ok : ran.stderr.replace(/^llmpool: /, '').trim() || 'llmpool could not do that')

const use = async ($: EngineInterface, session: string, row: Row) => {
  say($, await llmpool($, ['switch', row.id, '--session', session]), `Switching to ${row.label}. It takes effect within a minute.`)
  await refresh($)
}

const useNext = async ($: EngineInterface, session: string) => {
  const ran = await llmpool($, ['switch', '--next', '--session', session])
  say($, ran, ran.stdout.trim())
  await refresh($)
}

/** Start a sign-in in the browser; `email` pre-fills the page. */
const signIn = async ($: EngineInterface, email: string | null) => {
  const args = ['add', 'claude', '--detach', ...(email ? ['--email', email] : [])]
  say($, await llmpool($, args), 'Finish signing in in your browser. The account appears here when you are done.')
  lastLogin = 'waiting'
  await refresh($)
}

/** Pass on the code the sign-in page shows when it cannot return by itself. */
const sendCode = async ($: EngineInterface, code: string) => {
  if (code.trim() === '') {
    return
  }
  say($, await llmpool($, ['add', 'claude', '--detach', '--code', code.trim()]), 'Code sent. Finishing the sign-in…')
  await refresh($)
}

const cancelSignIn = async ($: EngineInterface) => {
  say($, await llmpool($, ['add', 'claude', '--cancel']), 'Sign-in cancelled.')
  await refresh($)
}

const addCurrent = async ($: EngineInterface) => {
  const ran = await llmpool($, ['add', 'claude', '--from-current'])
  say($, ran, ran.stdout.trim())
  await refresh($)
}

/** Do what fixes an issue. */
const fix = ($: EngineInterface, issue: Issue) => (issue.action === 'add_current' ? addCurrent($) : signIn($, issue.email))

// Opened with the focus, so a key acts straight away.
const openPane = ($: EngineInterface) => $.ui.open({ id: PANE, title: TITLE, focus: true, closeOnEscape: true })

/** `/llmpool next`, `/llmpool add`, `/llmpool <account>`: act without the pane. */
const byCommand = async ($: EngineInterface, wanted: string): Promise<string> => {
  if (wanted === 'add' || wanted === 'login') {
    await signIn($, null)

    return 'Sign-in started in your browser.'
  }
  const snap = await refresh($)
  if (snap.session === null) {
    return snap.error ?? "This session's login is not in the pool yet. Run /llmpool and add it."
  }
  const target = wanted === 'next' ? ['--next'] : [wanted]
  const ran = await llmpool($, ['switch', ...target, '--session', snap.session])
  await refresh($)

  return ran.exitCode === 0 ? ran.stdout.trim() : `Could not switch: ${ran.stderr.replace(/^llmpool: /, '').trim()}`
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'llmpool',
      description: 'Accounts and usage; switch this session, or sign in to another account',
      argumentHint: '[next | add | <account>]',
    })
    void refresh($)
    $.clock.every(WAITING_MS, async () => {
      // One timer, two paces: often while a sign-in waits, seldom otherwise.
      const waiting = (await read($, snapshot)).login.state === 'waiting'
      tick = (tick + 1) % Math.round(REFRESH_MS / WAITING_MS)
      if (waiting || tick === 0) {
        await refresh($)
      }
    })

    return next(e)
  })

  let tick = 0

  on('command.run', { command: 'llmpool' }, async ($, e) => {
    // `/llmpool switch work-2` reads naturally, so the word is allowed;
    // `/llmpool switch` alone means "let me choose".
    const wanted = (e.args ?? '').trim().replace(/^switch\b\s*/, '')
    if (wanted !== '') {
      return { text: await byCommand($, wanted) }
    }
    await refresh($)
    await openPane($)

    return { text: 'llmpool: number keys switch · n next · a add an account · r refresh · esc closes' }
  })

  // Usage moves when a turn ends; show it without waiting for the timer.
  on('turn.complete', async ($, e, next) => {
    void refresh($)

    return next(e)
  })

  // The way in, on the hint line that is there anyway: no row of its own.
  on('ui.render', { component: 'PromptHint' }, async ($, e, next) => {
    if (e.props.isDraft || e.props.isWorking) {
      return next(e)
    }
    const snap = await read($, snapshot)
    const tail = snap.error !== null ? '' : snap.rows.length === 0 ? '/llmpool to add an account' : '/llmpool to switch account'

    return tail === '' ? next(e) : next({ ...e, props: { ...e.props, tail: [e.props.tail, tail].filter(Boolean).join(' · ') } })
  })

  // A band only when something needs the person: the current account is
  // running out, or its login is missing from the pool or has ended. The way
  // out is on the band itself.
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const snap = await read($, snapshot)
    if (e.props.hasSurvey || snap.error !== null) {
      return next(e)
    }
    const { Box, Button, Text } = $.ui.resolve(e)
    const problem = urgent(snap)
    if (problem !== undefined && snap.login.state !== 'waiting') {
      return (
        <Box gap={1}>
          <Text color="warning" bold>
            ⚠ {problem.text}
          </Text>
          {fixLabel(problem) !== '' && <Button key="band-fix" label={fixLabel(problem)} onPress={() => fix($, problem)} />}
        </Box>
      )
    }
    const row = snap.rows.find(r => r.isCurrent)
    if (!row || peak(row) < WARN_AT) {
      return next(e)
    }
    const full = fullest(row)
    const tone = level(full.percent)
    const target = snap.session === null ? undefined : best(snap)

    return (
      <Box gap={1}>
        <Text color={tone.color} bold>
          ⚠ {row.label}: {Math.round(full.percent)}% of the {full.name} limit
        </Text>
        <Text dimColor>{resetsIn(full.resetAt, await $.clock.now())}</Text>
        {target !== undefined && (
          <Button key="band-switch" label={`Switch to ${target.label}`} onPress={() => use($, snap.session ?? '', target)} />
        )}
        <Button key="band-open" label="Accounts" onPress={() => void openPane($)} />
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Button, Input, Text } = $.ui.resolve(e)
    const snap = await read($, snapshot)
    const now = await $.clock.now()
    const canSwitch = snap.session !== null
    const waiting = snap.login.state === 'waiting'

    if (snap.error !== null) {
      return (
        <Box flexDirection="column" gap={1}>
          <Text color="error" bold>
            ⚠ llmpool is not answering
          </Text>
          <Text>{snap.error}</Text>
          <Text dimColor>Check that `llmpool doctor` works in a terminal, then press r.</Text>
          <Button key="refresh" plain hotkey="r" label="Try again" onPress={() => refresh($)} />
        </Box>
      )
    }

    const meter = (name: string, percent: number | null, resetAt: number | null) => {
      const tone = level(percent)

      return (
        <Box gap={1}>
          <Text dimColor>{name}</Text>
          <Text color={tone.color}>{bar(percent)}</Text>
          <Text color={tone.color} bold>
            {percentText(percent)}
          </Text>
          <Text color={tone.color}>{tone.word}</Text>
          <Text dimColor>{resetsIn(resetAt, now)}</Text>
        </Box>
      )
    }

    // Digits switch: the first nine accounts that can be switched to.
    const hotkeys = new Map<string, string>()
    for (const row of snap.rows.filter(r => !r.isCurrent && isUsable(r)).slice(0, 9)) {
      hotkeys.set(row.id, String(hotkeys.size + 1))
    }

    const card = (row: Row) => {
      const mark = stateMark(row)
      const usable = isUsable(row)
      const needsLogin = row.state !== 'ok' && row.state !== 'disabled'

      return (
        <Box flexDirection="column" marginTop={1}>
          <Box gap={1}>
            <Text color={row.isCurrent ? 'success' : usable || row.isElsewhere ? 'text' : 'warning'} bold={row.isCurrent}>
              {mark.icon} {row.label}
            </Text>
            {mark.text !== '' && (
              <Text color={row.isCurrent ? 'success' : row.isElsewhere ? 'inactive' : 'warning'}>
                {needsLogin ? 'needs a new sign-in' : mark.text}
              </Text>
            )}
            {row.tags.length > 0 && <Text dimColor>{row.tags.map(tag => `#${tag}`).join(' ')}</Text>}
          </Box>
          {(usable || row.isCurrent || row.isElsewhere) && (
            <Box flexDirection="column" marginLeft={2}>
              {meter('5h', row.h5, row.reset5)}
              {meter('7d', row.d7, row.reset7)}
              {canSwitch && hotkeys.has(row.id) && (
                <Button
                  key={`use-${row.id}`}
                  plain
                  hotkey={hotkeys.get(row.id)}
                  label={`Switch this session to ${row.label}`}
                  onPress={() => use($, snap.session ?? '', row)}
                />
              )}
            </Box>
          )}
        </Box>
      )
    }

    // What is missing comes first, each with the press that fixes it.
    const issues = snap.issues.filter(i => fixLabel(i) !== '' || i.action === 'install' || i.action === 'none')

    return (
      <Box flexDirection="column">
        {waiting && (
          <Box flexDirection="column">
            <Text color="suggestion" bold>
              ◌ Waiting for you to finish signing in in your browser
            </Text>
            {snap.login.url !== null && <Text dimColor>If no page opened: {snap.login.url}</Text>}
            <Input
              key="login-code"
              label="Code"
              placeholder="only if the page shows a code: paste it here, then Enter"
              onSubmit={value => sendCode($, value)}
            />
            <Button key="login-cancel" plain hotkey="c" label="Cancel the sign-in" onPress={() => cancelSignIn($)} />
          </Box>
        )}
        {!waiting &&
          issues.map((issue, index) => (
            <Box flexDirection="column" marginBottom={1}>
              <Text color="warning">⚠ {issue.text}</Text>
              {fixLabel(issue) !== '' && (
                <Button key={`fix-${index}`} plain hotkey={index === 0 ? 'f' : undefined} label={fixLabel(issue)} onPress={() => fix($, issue)} />
              )}
            </Box>
          ))}
        {snap.rows.length === 0 && issues.length === 0 && <Text dimColor>No accounts in the pool yet.</Text>}
        {snap.rows.map(card)}
        {snap.others.length > 0 && (
          <Box flexDirection="column" marginTop={1}>
            <Text dimColor bold>
              Other sessions ({snap.others.length})
            </Text>
            {snap.others.slice(0, 6).map(s => (
              <Box gap={1} marginLeft={2}>
                <Text dimColor>{s.label}</Text>
                <Text dimColor wrap="truncate-start">
                  {s.cwd}
                </Text>
              </Box>
            ))}
          </Box>
        )}
        <Box flexDirection="column" marginTop={1}>
          <Text dimColor bold>
            Actions
          </Text>
          {canSwitch && hotkeys.size > 0 && (
            <Button key="next" plain hotkey="n" label="Switch to the account with the most quota" onPress={() => useNext($, snap.session ?? '')} />
          )}
          {!waiting && <Button key="add" plain hotkey="a" label="Add an account (signs in through your browser)" onPress={() => signIn($, null)} />}
          <Button key="refresh" plain hotkey="r" label="Refresh" onPress={() => refresh($)} />
        </Box>
      </Box>
    )
  })
}
