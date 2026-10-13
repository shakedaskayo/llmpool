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
  /** Its login is live where this session cannot share it. */
  isElsewhere: boolean
  /** Percent of the 5-hour window over time, oldest first, for a trend line. */
  trend: number[]
}

/** The pool as a whole. */
export type Stats = {
  /** Percent of the combined allowance still unused; null with no usable account. */
  headroom5: number | null
  headroom7: number | null
  switches24h: number
  lastSwitch: { at: number; text: string } | null
}

/** Something that stands in the way, and what fixes it. */
export type Issue = {
  kind: string
  text: string
  /** "add_current", "login", "install" or "none". */
  action: string
  email: string | null
}

/** A sign-in under way in the browser. */
export type Login = {
  /** "idle", "waiting", "done", "failed" or "cancelled". */
  state: string
  message: string
  url: string | null
}

export type Snapshot = {
  /** This session's llmpool id; null when llmpool does not manage it. */
  session: string | null
  /** Why this session runs on its account: the rule that chose it, or "you chose it". */
  pickedBy: string | null
  rows: Row[]
  others: { id: string; label: string; cwd: string }[]
  error: string | null
  issues: Issue[]
  login: Login
  stats: Stats
  config: Config
  /** What the advisor last said about this session, in one line; '' when nothing. */
  advice: string
}

/** The switches that can be thrown from here. */
export type Config = {
  preferOwn: boolean
  advisorOn: boolean
  advisorHasKey: boolean
}

declare module 'claude-code' {
  interface PluginState {
    llmpool: { snapshot: Snapshot }
  }
}
