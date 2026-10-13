import type { Row, Snapshot } from '../types'

type Json = Record<string, any>

const windowOf = (row: Json, name: string): Json | undefined => row?.usage?.windows?.[name]

/** Reduce `llmpool status --json` to what the pane draws. */
export const toSnapshot = (status: Json, session: string | null): Snapshot => {
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
  }))
  const others = (status?.sessions ?? [])
    .filter((s: Json) => s.id !== session)
    .map((s: Json) => ({ id: s.id, label: s.account_label ?? 'default login', cwd: s.cwd }))

  return { session: status?.session ? session : null, rows, others, error: null }
}

/** A ten-cell meter. */
export const bar = (percent: number | null): string => {
  if (percent === null) {
    return '··········'
  }
  const filled = Math.max(0, Math.min(10, Math.round(percent / 10)))

  return '█'.repeat(filled) + '░'.repeat(10 - filled)
}

export const percentText = (percent: number | null): string =>
  percent === null ? '  -' : `${Math.round(percent)}%`.padStart(4)

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

/** One line for the status area: the current account and how full it is. */
export const statusText = (snap: Snapshot): string | undefined => {
  const row = snap.rows.find(r => r.isCurrent)
  if (!row) {
    return undefined
  }
  const parts = [`llmpool: ${row.label}`]
  if (row.h5 !== null) {
    parts.push(`5h ${Math.round(row.h5)}%`)
  }
  if (row.d7 !== null) {
    parts.push(`7d ${Math.round(row.d7)}%`)
  }

  return parts.join(' · ')
}

/** The fuller of the two windows, for deciding when to warn. */
export const peak = (row: Row): number => Math.max(row.h5 ?? 0, row.d7 ?? 0)
