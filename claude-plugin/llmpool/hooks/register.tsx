import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { Row, Snapshot } from '../types'
import {
  bar,
  best,
  fullest,
  isUsable,
  level,
  peak,
  percentText,
  resetsIn,
  stateMark,
  statusText,
  toSnapshot,
} from './view'

// `llmpool install claude` replaces this with the installed binary's path.
const INSTALLED = '__LLMPOOL_BIN__'
const BIN = INSTALLED.startsWith('__') ? 'llmpool' : INSTALLED

const PANE = 'llmpool'
const TITLE = 'llmpool · accounts'
const REFRESH_MS = 15000
/** Usage at which the band above the prompt appears. */
const WARN_AT = 80

const EMPTY: Snapshot = { session: null, rows: [], others: [], error: null }
const snapshot = atom({ plugin: 'llmpool', key: 'snapshot' } as const, EMPTY)

let isAnnounced = false

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
  const text = statusText(next)
  $.ui.status(text)
  if (text !== undefined && !isAnnounced) {
    isAnnounced = true
    $.ui.toast(`${text} · /llmpool to switch`)
  }
}

const use = async ($: EngineInterface, session: string, row: Row) => {
  const ran = await llmpool($, ['switch', row.id, '--session', session])
  $.ui.toast(
    ran.exitCode === 0
      ? `Switching to ${row.label}. It takes effect within a minute.`
      : `Could not switch: ${ran.stderr.replace(/^llmpool: /, '').trim()}`,
  )
  await refresh($)
}

// Opened with the focus, so a number key switches straight away.
const openPane = ($: EngineInterface) => $.ui.open({ id: PANE, title: TITLE, focus: true, closeOnEscape: true })

/** `/llmpool next`, `/llmpool <account>`: switch without opening the pane. */
const switchByCommand = async ($: EngineInterface, wanted: string): Promise<string> => {
  await refresh($)
  const snap = await read($, snapshot)
  if (snap.session === null) {
    return snap.error ?? "This session's login is not in the pool. Add it with `llmpool add claude --from-current`."
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
      description: 'Show llmpool accounts and usage, and switch this session to another account',
      argumentHint: '[next | <account>]',
    })
    void refresh($)
    $.clock.every(REFRESH_MS, () => refresh($))

    return next(e)
  })

  on('command.run', { command: 'llmpool' }, async ($, e) => {
    // `/llmpool switch work-2` reads naturally, so the word is allowed;
    // `/llmpool switch` alone means "let me choose".
    const wanted = (e.args ?? '').trim().replace(/^switch\b\s*/, '')
    if (wanted !== '') {
      return { text: await switchByCommand($, wanted) }
    }
    await refresh($)
    await openPane($)

    return { text: 'llmpool accounts pane opened. Press a number to switch, r to refresh.' }
  })

  // Usage moves when a turn ends; show it without waiting for the timer.
  on('turn.complete', async ($, e, next) => {
    void refresh($)

    return next(e)
  })

  // A band only when the current account is running out, with the way out on
  // it: one press to the account that has the most room.
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const snap = await read($, snapshot)
    const row = snap.rows.find(r => r.isCurrent)
    if (e.props.hasSurvey || !row || peak(row) < WARN_AT) {
      return next(e)
    }
    const { Box, Button, Text } = $.ui.resolve(e)
    const full = fullest(row)
    const tone = level(full.percent)
    const reset = resetsIn(full.resetAt, await $.clock.now())
    const target = snap.session === null ? undefined : best(snap)

    return (
      <Box gap={1}>
        <Text color={tone.color} bold>
          ⚠ {row.label}: {Math.round(full.percent)}% of the {full.name} limit
        </Text>
        <Text dimColor>{reset}</Text>
        {target !== undefined && (
          <Button key="band-switch" label={`Switch to ${target.label}`} onPress={() => use($, snap.session ?? '', target)} />
        )}
        <Button key="band-open" label="Accounts" onPress={() => void openPane($)} />
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Button, Text } = $.ui.resolve(e)
    const snap = await read($, snapshot)
    const now = await $.clock.now()
    const canSwitch = snap.session !== null

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

      return (
        <Box flexDirection="column" marginTop={1}>
          <Box gap={1}>
            <Text color={row.isCurrent ? 'success' : usable ? 'text' : 'warning'} bold={row.isCurrent}>
              {mark.icon} {row.label}
            </Text>
            {mark.text !== '' && <Text color={row.isCurrent ? 'success' : 'warning'}>{mark.text}</Text>}
            {row.tags.length > 0 && <Text dimColor>{row.tags.map(tag => `#${tag}`).join(' ')}</Text>}
            {row.sessions > 0 && !row.isCurrent && (
              <Text dimColor>
                in use by {row.sessions} session{row.sessions > 1 ? 's' : ''}
              </Text>
            )}
          </Box>
          {(usable || row.isCurrent) && (
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

    if (snap.error !== null) {
      return (
        <Box flexDirection="column" gap={1}>
          <Text color="error" bold>
            ⚠ llmpool is not answering
          </Text>
          <Text>{snap.error}</Text>
          <Text dimColor>Check that `llmpool list` works in a terminal, then press r.</Text>
          <Button key="refresh" plain hotkey="r" label="Try again" onPress={() => refresh($)} />
        </Box>
      )
    }

    if (snap.rows.length === 0) {
      return (
        <Box flexDirection="column" gap={1}>
          <Text bold>No accounts in the pool yet</Text>
          <Text>Add the login this Claude Code uses now:</Text>
          <Text color="suggestion">  llmpool add claude --from-current</Text>
          <Text>Add another account (opens a login in a scratch profile):</Text>
          <Text color="suggestion">  llmpool add claude</Text>
          <Button key="refresh" plain hotkey="r" label="Refresh" onPress={() => refresh($)} />
        </Box>
      )
    }

    return (
      <Box flexDirection="column">
        {!canSwitch && (
          <Text color="warning">
            ⚠ This session's login is not in the pool, so it cannot be switched here. Add it with `llmpool add claude
            --from-current`.
          </Text>
        )}
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
        <Box marginTop={1} gap={2}>
          <Button key="refresh" plain hotkey="r" label="Refresh" onPress={() => refresh($)} />
          {hotkeys.size > 0 && canSwitch && <Text dimColor>Press a number to switch · or type /llmpool next</Text>}
        </Box>
      </Box>
    )
  })
}
