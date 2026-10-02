// pm's never-idle loop for opencode v2, which has no Stop hook: when the
// agent's turn ends, block in `pm harness hooks stop` until its inbox has a
// message, then prompt the session with the hook's continuation text.
//
// Requires `opencode --standalone`. A shared server carries the environment
// of whichever client started it, so a second agent would be driven under
// the first one's PM_AGENT_NAME.
//
// It also stands in for the hooks opencode lacks: it reports the user's
// prompts, the dialogs that wait on the user, and the agent's activity.
//
// Installed by pm and overwritten on upgrade. opencode reloads it in every
// running server when the file changes, so the cleanup must leave nothing
// waiting and setup must arm again.
//
// No `@opencode/plugin` import: it does not resolve for a local plugin.
import type { ChildProcess } from "node:child_process"
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import {
  ACTIVITY_FILE,
  Asks,
  INBOX_ENQUEUED,
  LOADED_FILE,
  Loop,
  PM_PROMPT,
  TURN_END,
  TURN_ERROR_FILE,
  TURN_STARTED,
  answeredOf,
  askOf,
  drivesSession,
  userInput,
} from "./loop.ts"
import { runPm } from "./pm.ts"

export default {
  id: "pm.never-idle",
  setup(ctx: any) {
    const agent = process.env.PM_AGENT_NAME
    if (!agent) return

    const own = process.env.PM_OPENCODE_SESSION
    const appendFile = process.env.PM_APPEND_PROMPT_FILE
    const tripFile = process.env.PM_OPENCODE_TRIP_FILE
    const controller = new AbortController()
    const children = new Set<ChildProcess>()
    const pm = (args: string[], stdin: string, signal?: AbortSignal) =>
      runPm(args, stdin, { cwd: ctx.location.directory, env: process.env, children, signal })

    const stateFile = (name: string) => (tripFile ? join(dirname(tripFile), name) : undefined)
    const record = (file: string | undefined, text: string | null) => {
      if (!file) return
      try {
        if (text === null) return rmSync(file, { force: true })
        mkdirSync(dirname(file), { recursive: true })
        writeFileSync(file, text + "\n")
      } catch {}
    }
    const loadedFile = stateFile(LOADED_FILE)
    const turnErrorFile = stateFile(TURN_ERROR_FILE)
    const activityFile = stateFile(ACTIVITY_FILE)
    const active = () => record(activityFile, "")
    const asks = new Asks()
    // In order, so a reply can't overtake the ask it answers.
    let reported: Promise<unknown> = Promise.resolve()
    const waiting = (payload: object) => {
      reported = reported.then(() => pm(["harness", "hooks", "waiting", "opencode"], JSON.stringify(payload)))
    }
    // A restart's new TUI may write its marker before this one's cleanup runs.
    const loadedMark = `${process.pid} ${new Date().toISOString()}`

    record(tripFile, null)

    const loop = new Loop({
      agent,
      // Always `{}`: the plugin never holds a turn open, so the hook has no
      // running background work to yield to.
      hook: (cancel) => pm(["harness", "hooks", "stop"], "{}", cancel),
      prompt: (sessionID, text) => ctx.session.prompt({ sessionID, text, metadata: PM_PROMPT }),
      sleep: (ms) =>
        new Promise((resolve) => {
          const timer = setTimeout(resolve, ms)
          controller.signal.addEventListener("abort", () => {
            clearTimeout(timer)
            resolve()
          })
        }),
      report: (reason) => record(tripFile, reason),
      lastTurn: (error) => record(turnErrorFile, error),
    })

    // The last text read stands in for a file deleted under a live agent.
    if (appendFile) {
      let appended = ""
      const read = () => {
        try {
          appended = readFileSync(appendFile, "utf8")
        } catch {}
        return appended
      }
      read()
      void ctx.session.hook("context", (event: any) => {
        if (controller.signal.aborted) return
        const text = read()
        if (text) event.system.push({ type: "text", text })
      })
    }

    void ctx.tool.hook("execute.after", (event: any) => {
      if (controller.signal.aborted) return
      active()
      loop.toolRan(event.tool, event.input?.command, event.result)
    })

    // pm creates the session before launching, and neither that nor a
    // resume emits an event to arm on.
    if (own) {
      void (async () => {
        await pm(["harness", "hooks", "session-start"], JSON.stringify({ session_id: own }))
        void loop.arm(own)
      })()
    }

    void (async () => {
      for (;;) {
        let ended = "the stream closed"
        try {
          for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
            loop.eventReceived()
            const parentOf = async (id: string) => (await ctx.session.get({ sessionID: id })).parentID
            // opencode has no hooks for dialogs; these stand in for them.
            const ask = askOf(event)
            if (ask) {
              if (await drivesSession(own, ask.sessionID, parentOf)) {
                asks.opened(ask.id)
                waiting(ask.payload)
              }
              continue
            }
            const answered = answeredOf(event)
            if (answered) {
              if (asks.closed(answered)) waiting({ hook_event_name: "Resolved" })
              continue
            }
            const sessionID = event.data?.sessionID as string | undefined
            if (!sessionID) continue
            if (event.type === "session.created" && !own && !event.data?.parentID) {
              await pm(["harness", "hooks", "session-start"], JSON.stringify({ session_id: sessionID }))
            }
            if (event.type === INBOX_ENQUEUED) {
              // opencode has no UserPromptSubmit hook; this stands in for it.
              const text = userInput(event.data?.item)
              if (text !== null && (await drivesSession(own, sessionID, parentOf))) {
                void pm(["harness", "hooks", "user-prompt"], JSON.stringify({ prompt: text }))
              }
              continue
            }
            if (event.type === TURN_STARTED) {
              if (await drivesSession(own, sessionID, parentOf)) loop.turnStarted(sessionID)
              continue
            }
            if (!TURN_END.has(event.type)) continue
            if (await drivesSession(own, sessionID, parentOf)) {
              active()
              void loop.turnEnded(sessionID, event.type, event.data?.error)
            }
          }
        } catch (e) {
          ended = String(e)
        }
        if (!(await loop.subscriptionEnded(own, ended))) return
      }
    })()

    // Last, so a setup that throws leaves no proof of having loaded.
    record(loadedFile, loadedMark)

    return () => {
      try {
        if (loadedFile && readFileSync(loadedFile, "utf8").trim() === loadedMark) record(loadedFile, null)
      } catch {}
      loop.unload()
      controller.abort()
      for (const child of children) child.kill("SIGTERM")
      children.clear()
    }
  },
}
