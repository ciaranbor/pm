// pm's never-idle loop for opencode v2, which has no Stop hook: when the
// agent's turn ends, block in `pm harness hooks stop` until its inbox has a
// message, then prompt the session with the hook's continuation text.
//
// Requires `opencode --standalone`. A shared server carries the environment
// of whichever client started it, so a second agent would be driven under
// the first one's PM_AGENT_NAME.
//
// Installed by pm and overwritten on upgrade. opencode reloads it in every
// running server when the file changes, so the cleanup must leave nothing
// waiting and setup must arm again.
//
// No `@opencode/plugin` import: it does not resolve for a local plugin.
import type { ChildProcess } from "node:child_process"
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { dirname } from "node:path"
import { INBOX_ENQUEUED, Loop, PM_PROMPT, TURN_END, drivesSession, userInput } from "./loop.ts"
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
    const pm = (args: string[], stdin: string) =>
      runPm(args, stdin, { cwd: ctx.location.directory, env: process.env, children })

    if (tripFile) rmSync(tripFile, { force: true })

    const loop = new Loop({
      agent,
      // Always `{}`: the plugin never holds a turn open, so the hook has no
      // running background work to yield to.
      hook: () => pm(["harness", "hooks", "stop"], "{}"),
      prompt: (sessionID, text) => ctx.session.prompt({ sessionID, text, metadata: PM_PROMPT }),
      sleep: (ms) =>
        new Promise((resolve) => {
          const timer = setTimeout(resolve, ms)
          controller.signal.addEventListener("abort", () => {
            clearTimeout(timer)
            resolve()
          })
        }),
      report: (reason) => {
        if (!tripFile) return
        try {
          mkdirSync(dirname(tripFile), { recursive: true })
          writeFileSync(tripFile, reason + "\n")
        } catch {}
      },
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
            const sessionID = event.data?.sessionID as string | undefined
            if (!sessionID) continue
            if (event.type === "session.created" && !own && !event.data?.parentID) {
              await pm(["harness", "hooks", "session-start"], JSON.stringify({ session_id: sessionID }))
            }
            const parentOf = async (id: string) => (await ctx.session.get({ sessionID: id })).parentID
            if (event.type === INBOX_ENQUEUED) {
              // opencode has no UserPromptSubmit hook; this stands in for it.
              const text = userInput(event.data?.item)
              if (text !== null && (await drivesSession(own, sessionID, parentOf))) {
                void pm(["harness", "hooks", "user-prompt"], JSON.stringify({ prompt: text }))
              }
              continue
            }
            if (!TURN_END.has(event.type)) continue
            if (await drivesSession(own, sessionID, parentOf)) {
              void loop.turnEnded(sessionID, event.type, event.data?.error)
            }
          }
        } catch (e) {
          ended = String(e)
        }
        if (!(await loop.subscriptionEnded(own, ended))) return
      }
    })()

    return () => {
      loop.unload()
      controller.abort()
      for (const child of children) child.kill("SIGTERM")
      children.clear()
    }
  },
}
