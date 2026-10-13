import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { Snapshot } from '../types'
import { bar, peak, percentText, resetsIn, statusText, toSnapshot } from './view'

// `llmpool install claude` replaces this with the installed binary's path.
const INSTALLED = '__LLMPOOL_BIN__'
const BIN = INSTALLED.startsWith('__') ? 'llmpool' : INSTALLED

const PANE = 'llmpool'
const TITLE = 'llmpool'
const REFRESH_MS = 15000
/** Usage at which the band above the prompt appears. */
const WARN_AT = 80

const EMPTY: Snapshot = { session: null, rows: [], others: [], error: null }
const snapshot = atom({ plugin: 'llmpool', key: 'snapshot' } as const, EMPTY)

// Claude Code's id for this session, which is how llmpool finds a session it
// did not launch. Set when the session starts.
let agentSession = ''
let isAnnounced = false

const llmpool = ($: EngineInterface, args: string[]) => $.process.run([BIN, ...args], { timeoutMs: 8000 })

const refresh = async ($: EngineInterface) => {
  let next: Snapshot
  try {
    const ran = await llmpool($, ['status', '--json', '--agent-session', agentSession])
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

const use = async ($: EngineInterface, session: string, id: string, label: string) => {
  const ran = await llmpool($, ['switch', id, '--session', session])
  $.ui.toast(ran.exitCode === 0 ? `Switching to ${label}; takes effect within a minute` : ran.stderr.trim())
  await refresh($)
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'llmpool',
      description: 'Show llmpool accounts and usage, and switch this session to another account',
    })
    agentSession = e.session_id
    void refresh($)
    $.clock.every(REFRESH_MS, () => refresh($))

    return next(e)
  })

  on('command.run', { command: 'llmpool' }, async $ => {
    await refresh($)
    await $.ui.open({ id: PANE, title: TITLE })

    return { text: 'llmpool pane opened.' }
  })

  // Usage moves when a turn ends; show it without waiting for the timer.
  on('turn.complete', async ($, e, next) => {
    void refresh($)

    return next(e)
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const snap = await read($, snapshot)
    const row = snap.rows.find(r => r.isCurrent)
    if (e.props.hasSurvey || !row || peak(row) < WARN_AT) {
      return next(e)
    }
    const { Box, Button, Text } = $.ui.resolve(e)

    return (
      <Box>
        <Text color="yellow">
          llmpool: {row.label} is at {Math.round(peak(row))}% of its limit{' '}
        </Text>
        <Button key="open" label="Accounts" onPress={() => void $.ui.open({ id: PANE, title: TITLE })} />
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Button, Text } = $.ui.resolve(e)
    const snap = await read($, snapshot)
    const now = await $.clock.now()

    return (
      <Box flexDirection="column">
        {snap.error !== null && <Text color="red">{snap.error}</Text>}
        {snap.error === null && snap.session === null && (
          <Text dimColor>llmpool does not manage this session: its login is not in the pool. Add it with `llmpool add claude --from-current`.</Text>
        )}
        {snap.error === null && snap.rows.length === 0 && (
          <Text dimColor>No accounts yet. Add one with `llmpool add claude --from-current`.</Text>
        )}
        {snap.rows.map(row => (
          <Box flexDirection="column" marginTop={1}>
            <Box>
              <Text bold={row.isCurrent}>
                {row.isCurrent ? '● ' : '  '}
                {row.label}
              </Text>
              <Text dimColor>
                {row.tags.length > 0 ? `  [${row.tags.join(', ')}]` : ''}
                {row.state !== 'ok' ? `  ${row.state}` : ''}
                {row.sessions > 0 ? `  ${row.sessions} session${row.sessions > 1 ? 's' : ''}` : ''}
                {'  '}
              </Text>
              {snap.session !== null && !row.isCurrent && row.state === 'ok' && (
                <Button key={`use-${row.id}`} label="Switch" onPress={() => use($, snap.session ?? '', row.id, row.label)} />
              )}
            </Box>
            <Text>
              {'  5h '}
              {bar(row.h5)}
              {percentText(row.h5)} <Text dimColor>{resetsIn(row.reset5, now)}</Text>
            </Text>
            <Text>
              {'  7d '}
              {bar(row.d7)}
              {percentText(row.d7)} <Text dimColor>{resetsIn(row.reset7, now)}</Text>
            </Text>
          </Box>
        ))}
        {snap.others.length > 0 && (
          <Box flexDirection="column" marginTop={1}>
            <Text dimColor>Other sessions</Text>
            {snap.others.map(s => (
              <Text dimColor>
                {'  '}
                {s.id} {s.label} {s.cwd}
              </Text>
            ))}
          </Box>
        )}
        <Box marginTop={1}>
          <Button key="refresh" label="Refresh" onPress={() => refresh($)} />
        </Box>
      </Box>
    )
  })
}
