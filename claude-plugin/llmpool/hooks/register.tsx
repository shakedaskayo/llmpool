import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { Issue, Row, Snapshot } from '../types'
import {
  WINDOW_SECS,
  best,
  elapsed,
  fit,
  fixLabel,
  forecast,
  fullest,
  isUsable,
  level,
  miniBar,
  peak,
  percentText,
  quick,
  resetsIn,
  spark,
  stateMark,
  toSnapshot,
  urgent,
  wideBar,
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
  pickedBy: null,
  rows: [],
  others: [],
  error: null,
  issues: [],
  login: { state: 'idle', message: '', url: null },
  stats: { headroom5: null, headroom7: null, switches24h: 0, lastSwitch: null },
  config: { preferOwn: true, advisorOn: false, advisorHasKey: false },
  advice: '',
}
const snapshot = atom({ plugin: 'llmpool', key: 'snapshot' } as const, EMPTY)

// The quick-switch row and the pane opening by itself can each be turned off
// with `/llmpool row off` and `/llmpool pane off`; the choice is remembered.
let showRow = true
let autoPane = true

// What the last look at a sign-in said, so its ending is announced once.
let lastLogin = 'idle'
// Until the first answer from llmpool, nothing is known about the pool.
let isLoaded = false

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
  isLoaded = true
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

/** Throw one of the switches, then show the new state. */
const set = async ($: EngineInterface, args: string[], ok: string) => {
  say($, await llmpool($, args), ok)
  await refresh($)
}

/** Do what fixes an issue. */
const fix = ($: EngineInterface, issue: Issue) => (issue.action === 'add_current' ? addCurrent($) : signIn($, issue.email))

// Opened with the focus, so a key acts straight away.
const openPane = ($: EngineInterface) => $.ui.open({ id: PANE, title: TITLE, focus: true, closeOnEscape: true })

/** `/llmpool next`, `/llmpool add`, `/llmpool <account>`: act without the pane. */
const byCommand = async ($: EngineInterface, wanted: string): Promise<string> => {
  const toggle = wanted.match(/^(row|pane) (on|off)$/)
  if (toggle) {
    const isOn = toggle[2] === 'on'
    if (toggle[1] === 'row') {
      showRow = isOn
    } else {
      autoPane = isOn
    }
    await $.store.set(toggle[1] === 'row' ? 'showRow' : 'autoPane', isOn)

    return toggle[1] === 'row'
      ? `The quick-switch row above the prompt is ${isOn ? 'on' : 'off'}.`
      : `The accounts pane ${isOn ? 'opens' : 'no longer opens'} by itself on wide terminals.`
  }
  // `/llmpool team`, `/llmpool team <address>`, `/llmpool team sync|leave`.
  // A key is never typed here, where it would stay in the transcript.
  const team = wanted.match(/^team\b\s*(.*)$/)
  if (team) {
    const what = team[1].trim()
    const args =
      what === '' ? ['team'] : what === 'sync' ? ['team', 'sync'] : what === 'leave' ? ['team', 'disconnect'] : ['team', 'connect', what, '--detach']
    const ran = await llmpool($, args)
    await refresh($)

    return (ran.exitCode === 0 ? ran.stdout : ran.stderr).trim() || 'done'
  }
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
      argumentHint: '[next | add | <account> | row on|off | pane on|off]',
    })
    showRow = (await $.store.get('showRow')) !== false
    autoPane = (await $.store.get('autoPane')) !== false
    void refresh($)
    // Where there is room beside the conversation, the dashboard is simply
    // there. The engine seats an unasked pane only on wide terminals, and it
    // does not take the focus.
    if (autoPane) {
      void $.ui.open({ id: PANE, title: TITLE })
    }
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

    return { text: 'number keys switch · n next · a add an account · r refresh · esc closes' }
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
    const tail =
      snap.error !== null ? '' : !isLoaded ? '/llmpool' : snap.rows.length === 0 ? '/llmpool to add an account' : '/llmpool to switch account'

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
    const hot = row !== undefined && peak(row) >= WARN_AT
    const offers = showRow || hot ? quick(snap) : []
    // Nothing pressing and nowhere to switch to: no row at all.
    if (!hot && offers.length === 0) {
      return next(e)
    }
    const full = row === undefined ? null : fullest(row)

    return (
      <Box gap={1}>
        {hot && row !== undefined && full !== null ? (
          <Text color={level(full.percent).color} bold>
            ⚠ {row.label}: {Math.round(full.percent)}% of the {full.name} limit
          </Text>
        ) : (
          <Text dimColor>⇄ switch to</Text>
        )}
        {hot && full !== null && <Text dimColor>{resetsIn(full.resetAt, await $.clock.now())}</Text>}
        {offers.map((offer, index) => (
          <Button
            key={index === 0 ? 'band-switch' : `band-switch-${index}`}
            hotkey={String(index + 1)}
            label={`${offer.label} ${offer.h5 === null ? '' : `${Math.round(offer.h5)}%`}`.trim()}
            onPress={() => use($, snap.session ?? '', offer)}
          />
        ))}
        <Button key="band-open" hotkey="l" label="Accounts" onPress={() => void openPane($)} />
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const elements = $.ui.resolve(e)
    const { Box, Button, Text } = elements
    // Not every surface takes typed text; where it cannot, the field is left out.
    const Input = (elements as { Input?: (typeof elements)['Button'] }).Input as any
    const snap = await read($, snapshot)
    const now = await $.clock.now()
    const canSwitch = snap.session !== null
    const waiting = snap.login.state === 'waiting'
    const columns = e.viewport?.columns ?? 64
    const barWidth = Math.max(12, Math.min(36, columns - 30))
    const nameWidth = Math.max(10, Math.min(28, columns - 36))

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

    const heading = (text: string, note = '') => (
      <Box gap={1} marginTop={1}>
        <Text bold color="suggestion">
          {text}
        </Text>
        <Text dimColor>{note}</Text>
      </Box>
    )

    // One window of the current account, drawn large: a meter with the pace
    // mark, the number, the level in a word, and what the pace means.
    const big = (name: '5h' | '7d', percent: number | null, resetAt: number | null) => {
      const tone = level(percent)
      const pace = elapsed(resetAt, now, WINDOW_SECS[name])
      const note = [resetsIn(resetAt, now), forecast(percent, resetAt, now, WINDOW_SECS[name])].filter(Boolean).join(' · ')

      return (
        <Box flexDirection="column">
          <Box gap={1}>
            <Text dimColor>{name}</Text>
            <Box>
              {wideBar(percent, barWidth, pace).map(piece => (
                <Text color={piece.kind === 'used' ? tone.color : piece.kind === 'pace' ? 'text' : 'inactive'} bold={piece.kind === 'pace'}>
                  {piece.text}
                </Text>
              ))}
            </Box>
            <Text color={tone.color} bold>
              {percentText(percent)}
            </Text>
            <Text color={tone.color}>{tone.word}</Text>
          </Box>
          {note !== '' && (
            <Box marginLeft={3}>
              <Text dimColor>{note}</Text>
            </Box>
          )}
        </Box>
      )
    }

    const current = snap.rows.find(r => r.isCurrent)
    const others = snap.rows.filter(r => !r.isCurrent)

    // Digits switch: the first nine accounts that can be switched to.
    const hotkeys = new Map<string, string>()
    for (const row of others.filter(isUsable).slice(0, 9)) {
      hotkeys.set(row.id, String(hotkeys.size + 1))
    }

    // One account, one line: its key or mark, name, both windows as small
    // meters, and its recent trend.
    const line = (row: Row) => {
      const mark = stateMark(row)
      const key = canSwitch ? hotkeys.get(row.id) : undefined
      const measured = isUsable(row) || row.isElsewhere
      const five = level(row.h5)
      const seven = level(row.d7)

      return (
        <Box gap={1}>
          {key !== undefined ? (
            <Button key={`use-${row.id}`} plain hotkey={key} label={fit(row.label, nameWidth)} onPress={() => use($, snap.session ?? '', row)} />
          ) : (
            <Text color={measured ? 'text' : 'warning'}>
              {mark.icon}  {fit(row.label, nameWidth)}
            </Text>
          )}
          {measured ? (
            <Box gap={1}>
              <Text color={five.color}>
                {miniBar(row.h5)}
                {percentText(row.h5)}
              </Text>
              <Text color={seven.color}>
                {miniBar(row.d7)}
                {percentText(row.d7)}
              </Text>
              <Text dimColor>{spark(row.trend)}</Text>
              {row.isElsewhere && <Text dimColor>in use by another session</Text>}
            </Box>
          ) : (
            <Text color={row.state === 'disabled' ? 'inactive' : 'warning'}>
              {row.state === 'disabled' ? 'disabled' : 'needs a new sign-in'}
            </Text>
          )}
        </Box>
      )
    }

    const clock = (at: number) => {
      const d = new Date(at * 1000)

      return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
    }
    const free = (name: string, headroom: number | null) =>
      headroom === null ? null : (
        <Text color={level(100 - headroom).color}>
          {name} {miniBar(100 - headroom)} {Math.round(headroom)}% free
        </Text>
      )

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
            {Input && (
              <Input
                key="login-code"
                label="Code"
                placeholder="only if the page shows a code: paste it here, then Enter"
                onSubmit={(value: string) => sendCode($, value)}
              />
            )}
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

        {current !== undefined && (
          <Box flexDirection="column">
            {heading('THIS SESSION')}
            <Box gap={1}>
              <Text color="success" bold>
                ● {current.label}
              </Text>
              <Text color="success">this session</Text>
              {snap.pickedBy !== null && <Text dimColor>· {snap.pickedBy}</Text>}
            </Box>
            <Box>{snap.advice !== '' && <Text dimColor>{snap.advice}</Text>}
              {current.tags.length > 0 && <Text dimColor>{current.tags.map(tag => `#${tag}`).join(' ')}</Text>}
            </Box>
            {big('5h', current.h5, current.reset5)}
            {big('7d', current.d7, current.reset7)}
            {spark(current.trend) !== '' && (
              <Box gap={1}>
                <Text dimColor>trend</Text>
                <Text color={level(current.h5).color}>{spark(current.trend)}</Text>
                <Text dimColor>5-hour window, recent readings</Text>
              </Box>
            )}
            {(current.reset5 !== null || current.reset7 !== null) && <Text dimColor>┃ where even use would be by now</Text>}
          </Box>
        )}

        {others.length > 0 && (
          <Box flexDirection="column">
            {heading('POOL', `${snap.rows.length} accounts`)}
            <Text dimColor>
              {'   '}
              {fit('', nameWidth)} 5h{'         '}7d
            </Text>
            {others.map(line)}
          </Box>
        )}
        {snap.rows.length === 0 && issues.length === 0 && <Text dimColor>No accounts in the pool yet.</Text>}

        {snap.rows.length > 0 && (
          <Box flexDirection="column">
            {heading('TOTAL')}
            <Box gap={2}>
              {free('5h', snap.stats.headroom5)}
              {free('7d', snap.stats.headroom7)}
            </Box>
            <Text dimColor>
              {snap.stats.switches24h === 0 ? 'no switches' : `${snap.stats.switches24h} switch${snap.stats.switches24h > 1 ? 'es' : ''}`} in the last 24h
              {snap.stats.lastSwitch !== null ? ` · last at ${clock(snap.stats.lastSwitch.at)}` : ''}
              {snap.others.length > 0 ? ` · ${snap.others.length} other session${snap.others.length > 1 ? 's' : ''}` : ''}
            </Text>
            {snap.stats.lastSwitch !== null && (
              <Text dimColor wrap="truncate-end">
                {snap.stats.lastSwitch.text}
              </Text>
            )}
          </Box>
        )}

        <Box flexDirection="column">
          {heading('SETTINGS')}
          <Button
            key="set-own"
            plain
            hotkey="o"
            label={`Prefer my own account: ${snap.config.preferOwn ? 'on' : 'off'}`}
            onPress={() =>
              set(
                $,
                ['prefer-own', snap.config.preferOwn ? 'off' : 'on'],
                snap.config.preferOwn ? 'All accounts are treated alike.' : 'Your own account comes first while it has room.',
              )
            }
          />
          <Button
            key="set-advisor"
            plain
            hotkey="v"
            label={`Advisor: ${snap.config.advisorOn ? 'on' : 'off'}${snap.config.advisorHasKey ? '' : ' (no key: run `llmpool advisor key`)'}`}
            onPress={() =>
              set($, ['advisor', snap.config.advisorOn ? 'off' : 'on'], snap.config.advisorOn ? 'The advisor is off; your rules decide alone.' : 'The advisor is on.')
            }
          />
          {snap.config.advisorOn && snap.config.advisorHasKey && canSwitch && (
            <Button
              key="ask-advisor"
              plain
              hotkey="q"
              label="Ask the advisor about this session now"
              onPress={() => set($, ['advisor', 'ask', '--session', snap.session ?? ''], 'The advisor answered.')}
            />
          )}
          <Text dimColor>/llmpool row on|off · /llmpool pane on|off</Text>
        </Box>

        <Box flexDirection="column">
          {heading('ACTIONS')}
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
