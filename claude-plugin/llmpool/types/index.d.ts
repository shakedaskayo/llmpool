export type Row = {
  id: string
  label: string
  tags: string[]
  /** Percent used in the 5-hour window; null when there is no reading. */
  h5: number | null
  /** Percent used in the 7-day window; null when there is no reading. */
  d7: number | null
  /** Epoch seconds at which each window resets. */
  reset5: number | null
  reset7: number | null
  /** "ok", "disabled", or what is wrong with the login. */
  state: string
  sessions: number
  isCurrent: boolean
}

export type Snapshot = {
  /** This session's llmpool id; null when it was not started by `llmpool run`. */
  session: string | null
  rows: Row[]
  others: { id: string; label: string; cwd: string }[]
  error: string | null
}

declare module 'claude-code' {
  interface PluginState {
    llmpool: { snapshot: Snapshot }
  }
}
