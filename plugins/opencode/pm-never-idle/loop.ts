// The never-idle loop, free of the opencode runtime: what it waits on and
// what it does with the answer are handed in.

// Status files in the agent's runtime dir, beside the trip file pm names.
// pm reads them under these names.
export const LOADED_FILE = "opencode.loaded"
export const TURN_ERROR_FILE = "opencode.turn-error"
// Its mtime is the agent's last sign of life.
export const ACTIVITY_FILE = "activity"

export const TURN_FAILED = "session.execution.failed"

export const TURN_SUCCEEDED = "session.execution.succeeded"

export const TURN_END = new Set([
  TURN_SUCCEEDED,
  TURN_FAILED,
  "session.execution.interrupted",
])

// A failed turn usually fails again at once (model unavailable, credentials
// revoked), and the hook returns as fast while the message stays queued.
export const FAILURE_BACKOFF_MS = 30_000

// The hook answering this fast had a message queued before it was asked.
export const IMMEDIATE_MS = 500

export const MAX_WASTED_TURNS = 5

// Waits between attempts after the hook, or the event subscription, failed.
export const RETRY_MS = [1_000, 5_000, 15_000, 30_000]

export const MAX_FAILURES = RETRY_MS.length + 1

export type HookResult = { code: number | null; out: string }

// The header `pm msg read` prints above a message it took off the queue.
const MESSAGE_HEADER = /--- from \S+ \[\d+\] \d{4}-\d{2}-\d{2} /

const READ_COMMAND = /(^|[\s;&|(])pm msg read(\s|$)/

/**
 * Whether a tool call took a message off the agent's queue: a `pm msg read`
 * that printed one. `--index` re-reads without advancing, so it never counts.
 */
export function consumedMessage(tool: unknown, command: unknown, result: unknown): boolean {
  if (tool !== "shell") return false
  const line = String(command ?? "")
  if (!READ_COMMAND.test(line) || /(^|\s)--index(\s|=|$)/.test(line)) return false
  const output = typeof result === "string" ? result : JSON.stringify(result ?? "")
  return MESSAGE_HEADER.test(output)
}

/**
 * Stops the loop when it prompts turn after turn without the inbox draining.
 * Speed alone does not count: a sender that replies before the turn ends
 * always has the next message queued, so a healthy loop also sees the hook
 * answer at once. A turn is wasted only when it also read no message.
 */
export class Breaker {
  private wasted = 0
  private consumed = false
  private failure: string | null = null
  private reason: string | null = null
  private readonly max: number

  constructor(max: number = MAX_WASTED_TURNS) {
    this.max = max
  }

  get tripped(): string | null {
    return this.reason
  }

  noteConsumed(): void {
    this.consumed = true
  }

  /** The turn about to be closed failed with `error`. */
  noteFailed(error: string): void {
    this.failure = error
  }

  /**
   * Record one answer of the hook, `waitedMs` after it was asked at the end
   * of a turn. Returns the trip reason once the limit is reached.
   */
  turnClosed(waitedMs: number): string | null {
    const wasted = waitedMs < IMMEDIATE_MS && !this.consumed
    this.wasted = wasted ? this.wasted + 1 : 0
    const failure = this.failure
    this.consumed = false
    this.failure = null
    if (this.wasted >= this.max) {
      const read = `${this.wasted} consecutive turns were prompted for unread messages and read none`
      this.reason = failure
        ? `${read}; the last one failed: ${failure}`
        : `${read} (\`pm msg read\` may not be reaching this agent's inbox)`
    }
    return this.reason
  }
}

/**
 * The error a failed turn's event carries (`{type, message, status?}`), as
 * one line.
 */
export function turnError(error: unknown): string {
  const { type, message, status } = (error ?? {}) as Record<string, unknown>
  const detail = [type, status === undefined ? undefined : `HTTP ${status}`]
    .filter((part) => part !== undefined && part !== "")
    .join(", ")
  const text = typeof message === "string" && message ? message : "no error reported"
  return detail ? `${text} (${detail})` : text
}

export const INBOX_ENQUEUED = "session.inbox.enqueued"

/** Metadata on every prompt the plugin sends, so it is not taken for the user's. */
export const PM_PROMPT = { pm: "continuation" }

/**
 * The text of an enqueued inbox item (`session.inbox.enqueued`'s `item`) if
 * the user typed it: a `user` item the plugin did not send. Null otherwise.
 */
export function userInput(item: unknown): string | null {
  const { type, payload } = (item ?? {}) as { type?: unknown; payload?: any }
  if (type !== "user" || typeof payload?.text !== "string") return null
  if (payload.metadata?.pm === PM_PROMPT.pm) return null
  return payload.text
}

/**
 * Whether a turn end belongs to the session the loop drives. Turn ends fire
 * for subagent sessions too, and a finished subagent wakes its parent
 * natively, so prompting for one would double-deliver.
 */
export async function drivesSession(
  own: string | undefined,
  sessionID: string,
  parentOf: (sessionID: string) => Promise<string | undefined>,
): Promise<boolean> {
  if (own) return sessionID === own
  return !(await parentOf(sessionID))
}

/**
 * What the hook decided. The plugin only asks between turns, where the hook
 * has nothing to yield to, so anything but a block is a failure: `pm` could
 * not be run, exited with an error, said why its wait ended, or could not
 * tell which agent this is.
 */
export function hookDecision(result: HookResult): { block: string } | { failure: string } {
  if (result.code !== 0) {
    return { failure: result.code === null ? "killed" : `exit ${result.code}` }
  }
  let decision: any
  try {
    decision = JSON.parse(result.out)
  } catch {
    return { failure: `unreadable answer: ${result.out.trim().slice(0, 120)}` }
  }
  if (decision?.decision === "block" && typeof decision.reason === "string") {
    return { block: decision.reason }
  }
  if (typeof decision?.systemMessage === "string") {
    return { failure: decision.systemMessage }
  }
  return { failure: "it did not recognise this agent" }
}

export type LoopDeps = {
  agent: string
  /** Block in `pm harness hooks stop` and return its answer. */
  hook(): Promise<HookResult>
  prompt(sessionID: string, text: string): Promise<void>
  /** Resolves early when the plugin unloads. */
  sleep(ms: number): Promise<void>
  /** Record why the loop stopped where `pm doctor` finds it. */
  report(reason: string): void
  /**
   * Record where `pm doctor` finds it the error of a failed turn, or clear
   * it (null) after one that succeeded.
   */
  lastTurn(error: string | null): void
  now?(): number
}

export class Loop {
  private readonly deps: LoopDeps
  private readonly breaker = new Breaker()
  private readonly pumping = new Set<string>()
  private hookFailures = 0
  private subscriptionFailures = 0
  private unloaded = false
  private stoppedFor: string | null = null

  constructor(deps: LoopDeps) {
    this.deps = deps
  }

  get stopped(): string | null {
    return this.stoppedFor
  }

  /** Start waiting for a session no turn has ended in yet. */
  arm(sessionID: string): Promise<void> {
    return this.pump(sessionID, null)
  }

  /** `error` is what a failed turn's event carried. */
  turnEnded(sessionID: string, type: string, error?: unknown): Promise<void> {
    const failure = type === TURN_FAILED ? turnError(error) : null
    if (!this.unloaded && (failure !== null || type === TURN_SUCCEEDED)) this.deps.lastTurn(failure)
    return this.pump(sessionID, type, failure)
  }

  toolRan(tool: unknown, command: unknown, result: unknown): void {
    if (consumedMessage(tool, command, result)) this.breaker.noteConsumed()
  }

  unload(): void {
    this.unloaded = true
    this.pumping.clear()
  }

  eventReceived(): void {
    this.subscriptionFailures = 0
  }

  /**
   * The event subscription ended. Waits, then returns whether to subscribe
   * again; without events no turn end is ever seen, so giving up stops the
   * loop.
   */
  async subscriptionEnded(sessionID: string | undefined, detail: string): Promise<boolean> {
    if (this.unloaded || this.stoppedFor) return false
    this.subscriptionFailures += 1
    if (this.subscriptionFailures >= MAX_FAILURES) {
      await this.stop(sessionID, `its event subscription ended ${this.subscriptionFailures} times in a row (${detail})`)
      return false
    }
    await this.deps.sleep(RETRY_MS[this.subscriptionFailures - 1])
    return !this.unloaded
  }

  // One waiter per session: a turn ending while the hook is still blocked
  // (a prompt the user typed) must not start a second one. `turn` is the
  // event that ended a turn, or null when arming; `failure` is its error.
  private async pump(sessionID: string, turn: string | null, failure: string | null = null): Promise<void> {
    if (this.unloaded || this.stoppedFor || this.pumping.has(sessionID)) return
    this.pumping.add(sessionID)
    const now = this.deps.now ?? Date.now
    try {
      if (failure !== null) {
        this.breaker.noteFailed(failure)
        await this.deps.sleep(FAILURE_BACKOFF_MS)
      }
      for (;;) {
        if (this.unloaded) return
        const asked = now()
        const decision = hookDecision(await this.deps.hook())
        if (this.unloaded) return
        if ("failure" in decision) {
          this.hookFailures += 1
          if (this.hookFailures >= MAX_FAILURES) {
            return await this.stop(
              sessionID,
              `\`pm harness hooks stop\` failed ${this.hookFailures} times in a row (${decision.failure})`,
            )
          }
          await this.deps.sleep(RETRY_MS[this.hookFailures - 1])
          continue
        }
        this.hookFailures = 0
        const tripped = turn ? this.breaker.turnClosed(now() - asked) : null
        if (tripped) return await this.stop(sessionID, tripped)
        try {
          await this.deps.prompt(sessionID, decision.block)
        } catch (e) {
          await this.stop(sessionID, `the session could not be prompted (${String(e)})`)
        }
        return
      }
    } finally {
      this.pumping.delete(sessionID)
    }
  }

  private async stop(sessionID: string | undefined, reason: string): Promise<void> {
    this.stoppedFor = reason
    this.deps.report(reason)
    if (!sessionID) return
    const notice =
      `pm: never-idle loop stopped: ${reason}. This agent no longer wakes for messages; ` +
      `restart it with \`pm agent restart ${this.deps.agent}\`.`
    try {
      await this.deps.prompt(sessionID, notice)
    } catch {}
  }
}

/** What `pm harness hooks waiting opencode` reads. */
export type WaitingPayload = { hook_event_name: "PermissionRequest" | "Question" | "Resolved"; detail?: string }

/**
 * A dialog that opened, waiting on the user: a permission ask, or a form the
 * question tool raised. Null for any other event.
 */
export function askOf(event: { type?: unknown; data?: any }): { id: string; sessionID: string; payload: WaitingPayload } | null {
  const data = event.data ?? {}
  if (event.type === "permission.asked" && typeof data.id === "string") {
    const resources = Array.isArray(data.resources) ? data.resources.join(" ") : String(data.resources ?? "")
    return {
      id: data.id,
      sessionID: data.sessionID,
      payload: { hook_event_name: "PermissionRequest", detail: `${data.action ?? ""} ${resources}`.trim() },
    }
  }
  const form = data.form
  if (event.type === "form.created" && form?.metadata?.kind === "question" && typeof form.id === "string") {
    const label = Array.isArray(form.fields) ? form.fields[0]?.label : undefined
    return {
      id: form.id,
      sessionID: form.sessionID,
      payload: { hook_event_name: "Question", detail: String(form.title || label || "") },
    }
  }
  return null
}

/** The id of a dialog the user answered or dismissed; null for any other event. */
export function answeredOf(event: { type?: unknown; data?: any }): string | null {
  const data = event.data ?? {}
  if (event.type === "permission.replied" && typeof data.requestID === "string") return data.requestID
  if ((event.type === "form.replied" || event.type === "form.cancelled") && typeof data.id === "string") return data.id
  return null
}

/** The dialogs open in the sessions the loop drives; several can be at once. */
export class Asks {
  private readonly open = new Set<string>()

  opened(id: string): void {
    this.open.add(id)
  }

  /** Whether that closed the last one open. */
  closed(id: string): boolean {
    return this.open.delete(id) && this.open.size === 0
  }
}
