import type { Issue, Login, Row, Snapshot } from '../types'

type Json = Record<string, any>

const windowOf = (row: Json, name: string): Json | undefined => row?.usage?.windows?.[name]

/** The fuller of the two windows. */
export const peak = (row: Row): number => Math.max(row.h5 ?? 0, row.d7 ?? 0)

export const isUsable = (row: Row): boolean => row.state === 'ok' && !row.isElsewhere

/** Reduce `llmpool status --json` to what the pane draws: the current
 * account first, then accounts that can be switched to with the emptiest
 * first, then the ones that cannot. */
export const toSnapshot = (status: Json): Snapshot => {
  const session: string | null = status?.session?.id ?? null
  const current: string | null = status?.session?.account_id ?? null
  const rows: Row[] = (status?.accounts ?? []).map((a: Json) => ({
    id: a.id,
    label: a.label,
    tags: a.tags ?? [],
    h5: windowOf(a, '5h')?.used_percent ?? null,
    d7: windowOf(a, '7d')?.used_percent ?? null,
    reset5: windowOf(a, '5h')?.resets_at ?? null,
    reset7: windowOf(a, '7d')?.resets_at ?? null,
    state: a.enabled === false ? 'disabled' : String(a.health ?? 'ok').replace(/_/g, ' '),
    sessions: (a.sessions ?? []).length,
    isCurrent: a.id === current,
    // Live somewhere this session cannot share: in another session's
    // profile, or in the user's own while this one has a separate profile.
    isElsewhere:
      a.id !== current && ((a.sessions ?? []).length > 0 || (a.is_default === true && status?.session?.store !== 'default')),
  }))
  const rank = (row: Row) => (row.isCurrent ? 0 : isUsable(row) ? 1 : 2)
  rows.sort((a, b) => rank(a) - rank(b) || peak(a) - peak(b) || a.label.localeCompare(b.label))
  const others = (status?.sessions ?? [])
    .filter((s: Json) => s.id !== session)
    .map((s: Json) => ({ id: s.id, label: s.account_label ?? 'default login', cwd: s.cwd }))

  const issues: Issue[] = (status?.issues ?? []).map((i: Json) => ({
    kind: String(i.kind),
    text: String(i.text),
    action: String(i.action ?? 'none'),
    email: i.email ?? null,
  }))
  const login: Login = {
    state: String(status?.login?.state ?? 'idle'),
    message: String(status?.login?.message ?? ''),
    url: status?.login?.url ?? null,
  }

  return { session, rows, others, error: null, issues, login }
}

/** A ten-cell meter. */
export const bar = (percent: number | null): string => {
  if (percent === null) {
    return '▱▱▱▱▱▱▱▱▱▱'
  }
  const filled = Math.max(0, Math.min(10, Math.round(percent / 10)))

  return '▰'.repeat(filled) + '▱'.repeat(10 - filled)
}

export const percentText = (percent: number | null): string =>
  percent === null ? '  — ' : `${Math.round(percent)}%`.padStart(4)

export type Level = { color: 'success' | 'warning' | 'error' | 'inactive'; word: string }

/** How full a window is, as a colour and as a word, so the colour is never
 * the only thing that says it. */
export const level = (percent: number | null): Level => {
  if (percent === null) {
    return { color: 'inactive', word: 'no reading yet' }
  }
  if (percent >= 100) {
    return { color: 'error', word: 'limit reached' }
  }
  if (percent >= 90) {
    return { color: 'error', word: 'near limit' }
  }
  if (percent >= 70) {
    return { color: 'warning', word: 'filling up' }
  }

  return { color: 'success', word: 'plenty left' }
}

/** "resets in 2h 10m", or nothing when the time is unknown or past. */
export const resetsIn = (resetAt: number | null, nowMs: number): string => {
  if (resetAt === null) {
    return ''
  }
  const minutes = Math.floor((resetAt * 1000 - nowMs) / 60000)
  if (minutes <= 0) {
    return ''
  }
  const days = Math.floor(minutes / 1440)
  const hours = Math.floor((minutes % 1440) / 60)
  const text = days > 0 ? `${days}d ${hours}h` : hours > 0 ? `${hours}h ${minutes % 60}m` : `${minutes}m`

  return `resets in ${text}`
}

/** A mark and a word for an account that cannot be switched to. */
export const stateMark = (row: Row): { icon: string; text: string } => {
  if (row.isCurrent) {
    return { icon: '●', text: 'this session' }
  }
  if (row.state === 'ok') {
    return row.isElsewhere ? { icon: '◐', text: 'in use by another session' } : { icon: '○', text: '' }
  }
  if (row.state === 'disabled') {
    return { icon: '⏸', text: 'disabled · enable with `llmpool enable`' }
  }

  return { icon: '⚠', text: 'needs a new login · run `llmpool add claude`' }
}

/** The account worth switching to: usable, not the current one, with clearly
 * more room. Nothing when no account is meaningfully better. */
export const best = (snap: Snapshot): Row | undefined => {
  const current = snap.rows.find(r => r.isCurrent)
  if (!current) {
    return undefined
  }

  return snap.rows.find(r => !r.isCurrent && isUsable(r) && peak(r) + 10 <= peak(current))
}

/** Which window is the fuller one, for saying what is running out. */
export const fullest = (row: Row): { name: string; percent: number; resetAt: number | null } =>
  (row.d7 ?? 0) > (row.h5 ?? 0)
    ? { name: '7-day', percent: row.d7 ?? 0, resetAt: row.reset7 }
    : { name: '5-hour', percent: row.h5 ?? 0, resetAt: row.reset5 }

/** What a button that fixes an issue should say. */
export const fixLabel = (issue: Issue): string =>
  issue.action === 'add_current'
    ? 'Add this login to the pool'
    : issue.action === 'login'
      ? issue.email
        ? `Sign in as ${issue.email}`
        : 'Sign in'
      : ''

/** The issue worth interrupting for: one about the login this session runs on. */
export const urgent = (snap: Snapshot): Issue | undefined => {
  const current = snap.rows.find(r => r.isCurrent)

  return snap.issues.find(
    i => i.kind === 'login_not_in_pool' || (i.kind === 'relogin' && current !== undefined && i.text.startsWith(current.label)),
  )
}
