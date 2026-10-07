import assert from "node:assert/strict"
import { chmodSync, existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test, type TestContext } from "node:test"
import {
  Asks,
  Breaker,
  FAILURE_BACKOFF_MS,
  IMMEDIATE_MS,
  Loop,
  MAX_FAILURES,
  MAX_WASTED_TURNS,
  PM_PROMPT,
  RETRY_MS,
  RemoteAsks,
  TURN_FAILED,
  answeredOf,
  askOf,
  consumedMessage,
  drivesSession,
  failedTurnOf,
  hookDecision,
  turnError,
  userInput,
  type HookResult,
} from "./loop.ts"
import { runPm } from "./pm.ts"

const FAST = IMMEDIATE_MS - 1
const SUCCEEDED = "session.execution.succeeded"
const BLOCK: HookResult = { code: 0, out: '{"decision":"block","reason":"You have new messages"}' }

/** A loop over scripted hook answers, recording what it did. */
function harness(answers: HookResult[] | ((cancel: AbortSignal) => Promise<HookResult>)) {
  const seen = {
    prompts: [] as string[],
    sleeps: [] as number[],
    reports: [] as string[],
    lastTurn: [] as (string | null)[],
    asked: 0,
  }
  const queue = Array.isArray(answers) ? [...answers] : null
  const loop = new Loop({
    agent: "reviewer",
    hook: async (cancel) => {
      seen.asked += 1
      if (!queue) return (answers as (cancel: AbortSignal) => Promise<HookResult>)(cancel)
      const next = queue.shift()
      if (!next) throw new Error("the loop asked the hook more often than scripted")
      return next
    },
    prompt: async (_sessionID, text) => {
      seen.prompts.push(text)
    },
    sleep: async (ms) => {
      seen.sleeps.push(ms)
    },
    report: (reason) => seen.reports.push(reason),
    lastTurn: (error) => seen.lastTurn.push(error),
    now: () => 0,
  })
  return { loop, seen }
}

/** An executable standing in for `pm`, removed when the test ends. */
function fakePm(t: TestContext, script: string): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-plugin-test-"))
  t.after(() => rmSync(dir, { recursive: true, force: true }))
  const path = join(dir, "pm")
  writeFileSync(path, `#!/bin/sh\n${script}\n`)
  chmodSync(path, 0o755)
  return path
}

test("a loop that keeps prompting without a read trips at the limit", () => {
  const breaker = new Breaker()
  for (let turn = 1; turn < MAX_WASTED_TURNS; turn++) {
    assert.equal(breaker.turnClosed(FAST), null, `turn ${turn}`)
  }
  const reason = breaker.turnClosed(FAST)
  assert.match(reason ?? "", new RegExp(`^${MAX_WASTED_TURNS} consecutive turns`))
  assert.equal(breaker.tripped, reason)
})

test("a fast sender never trips a loop whose turns read their message", () => {
  const breaker = new Breaker()
  for (let turn = 0; turn < MAX_WASTED_TURNS * 10; turn++) {
    breaker.noteConsumed()
    assert.equal(breaker.turnClosed(0), null, `turn ${turn}`)
  }
  assert.equal(breaker.tripped, null)
})

test("a turn that read a message resets the count", () => {
  const breaker = new Breaker()
  for (let turn = 1; turn < MAX_WASTED_TURNS; turn++) breaker.turnClosed(FAST)
  breaker.noteConsumed()
  assert.equal(breaker.turnClosed(FAST), null)
  for (let turn = 1; turn < MAX_WASTED_TURNS; turn++) {
    assert.equal(breaker.turnClosed(FAST), null, `turn ${turn} after the reset`)
  }
  assert.notEqual(breaker.turnClosed(FAST), null)
})

test("a read counts for one turn only", () => {
  const breaker = new Breaker(2)
  breaker.noteConsumed()
  assert.equal(breaker.turnClosed(FAST), null)
  assert.equal(breaker.turnClosed(FAST), null)
  assert.notEqual(breaker.turnClosed(FAST), null)
})

test("a hook that blocked for a message is not a wasted turn", () => {
  const breaker = new Breaker()
  for (let turn = 0; turn < MAX_WASTED_TURNS * 3; turn++) {
    assert.equal(breaker.turnClosed(IMMEDIATE_MS), null, `turn ${turn}`)
  }
})

test("a read consumed a message only if it printed one and advanced the queue", () => {
  const message = "--- from driver [014] 2026-09-28 11:52:48 UTC ---\nping 4\n"
  const asToolResult = { content: [{ type: "text", text: message }], metadata: { exit: 0 } }
  assert.equal(consumedMessage("shell", "pm msg read", message), true)
  assert.equal(consumedMessage("shell", "pm msg read", asToolResult), true)
  assert.equal(consumedMessage("shell", "cd x && pm msg read --from main", message), true)
  assert.equal(
    consumedMessage("shell", "pm msg read", "--- from main@login [002] 2026-01-01 00:00:00 UTC ---\nhi"),
    true,
  )
  // A message may itself say what an empty inbox says.
  assert.equal(consumedMessage("shell", "pm msg read", message + "reply with: No new messages\n"), true)

  assert.equal(consumedMessage("shell", "pm msg read", "No new messages\n"), false)
  assert.equal(consumedMessage("shell", "pm msg read --from main", "No new messages from main"), false)
  assert.equal(consumedMessage("shell", "pm msg read", "error: not inside a pm project"), false)
  assert.equal(consumedMessage("shell", "pm msg read", ""), false)
  // Re-reads leave the queue as it was.
  assert.equal(consumedMessage("shell", "pm msg read --from main --index 3", message), false)
  assert.equal(consumedMessage("shell", "pm msg read --index=-1 --from main", message), false)
  // Other commands, whatever they print or mention.
  assert.equal(consumedMessage("shell", "pm msg list", message), false)
  assert.equal(consumedMessage("shell", "pm msg reads", message), false)
  assert.equal(consumedMessage("shell", "grep -r 'xpm msg read' .", message), false)
  assert.equal(consumedMessage("shell", "echo done", "run pm msg read"), false)
  assert.equal(consumedMessage("read", "pm msg read", message), false)
  assert.equal(consumedMessage("shell", undefined, undefined), false)
})

test("with a session of its own the loop drives that session and no other", async () => {
  const neverAsked = async () => {
    throw new Error("parent lookup is not needed when the session is known")
  }
  assert.equal(await drivesSession("ses_own", "ses_own", neverAsked), true)
  assert.equal(await drivesSession("ses_own", "ses_child", neverAsked), false)
})

test("without one it drives top-level sessions and skips subagent sessions", async () => {
  const parents: Record<string, string | undefined> = { ses_child: "ses_top", ses_top: undefined }
  const parentOf = async (id: string) => parents[id]
  assert.equal(await drivesSession(undefined, "ses_top", parentOf), true)
  assert.equal(await drivesSession(undefined, "ses_child", parentOf), false)
})

test("only text the user typed counts as their input, not the plugin's own prompts", () => {
  const user = (payload: object) => userInput({ type: "user", payload, delivery: "steer" })
  assert.equal(user({ text: "use postgres" }), "use postgres")
  assert.equal(user({ text: "use postgres", metadata: { source: "tui" } }), "use postgres")
  assert.equal(user({ text: "You have new messages", metadata: PM_PROMPT }), null)
  assert.equal(userInput({ type: "synthetic", payload: { text: "compacted" } }), null)
  assert.equal(userInput(undefined), null)
})

test("only a block from a hook that exited cleanly is a decision", () => {
  assert.deepEqual(hookDecision(BLOCK), { block: "You have new messages" })
  assert.deepEqual(hookDecision({ code: 0, out: "{}" }), { failure: "it did not recognise this agent" })
  assert.deepEqual(hookDecision({ code: 0, out: '{"decision":"block"}' }), {
    failure: "it did not recognise this agent",
  })
  assert.deepEqual(hookDecision({ code: 0, out: '{"systemMessage":"pm: Stop hook failed: x"}' }), {
    failure: "pm: Stop hook failed: x",
  })
  assert.deepEqual(hookDecision({ code: 0, out: "" }), { failure: "unreadable answer: " })
  assert.deepEqual(hookDecision({ code: 1, out: BLOCK.out }), { failure: "exit 1" })
  assert.deepEqual(hookDecision({ code: -1, out: "" }), { failure: "exit -1" })
  assert.deepEqual(hookDecision({ code: null, out: "" }), { failure: "killed" })
})

test("the loop prompts once per hook answer", async () => {
  const { loop, seen } = harness([BLOCK, BLOCK, BLOCK])
  await loop.arm("ses_1")
  for (const _ of [1, 2]) {
    loop.toolRan("shell", "pm msg read", "--- from main [001] 2026-01-01 00:00:00 UTC ---\nhi")
    await loop.turnEnded("ses_1", SUCCEEDED)
  }
  assert.deepEqual(seen.prompts, Array(3).fill("You have new messages"))
  assert.deepEqual(seen.reports, [])
  assert.equal(loop.stopped, null)
})

test("a turn ending while the hook is blocked does not start a second waiter", async () => {
  let release: (result: HookResult) => void = () => {}
  const { loop, seen } = harness(() => new Promise((resolve) => (release = resolve)))
  const waiting = loop.arm("ses_1")
  await loop.turnEnded("ses_1", SUCCEEDED)
  assert.equal(seen.asked, 1)
  release(BLOCK)
  await waiting
  assert.deepEqual(seen.prompts, ["You have new messages"])
})

/** A hook that blocks until cancelled, then answers as killed. */
function blockingHook() {
  return (cancel: AbortSignal) =>
    new Promise<HookResult>((resolve) => cancel.addEventListener("abort", () => resolve({ code: null, out: "" })))
}

test("a turn the plugin did not prompt cancels the wait, which is no failure, and its end waits again", async () => {
  let answers = 0
  const blocking = blockingHook()
  const { loop, seen } = harness((cancel) => (answers++ === 0 ? blocking(cancel) : Promise.resolve(BLOCK)))
  const waiting = loop.arm("ses_1")
  assert.equal(loop.turnStarted("ses_1"), true, "reported, the killed hook unable to")
  await waiting
  assert.deepEqual(seen.prompts, [])
  assert.deepEqual(seen.sleeps, [], "no retry after a cancelled wait")

  await loop.turnEnded("ses_1", SUCCEEDED)
  assert.equal(seen.asked, 2)
  assert.deepEqual(seen.prompts, ["You have new messages"])
  assert.deepEqual(seen.reports, [])
})

test("cancelled waits never stop the loop", async () => {
  const { loop, seen } = harness(blockingHook())
  for (let turn = 0; turn < MAX_FAILURES + 1; turn++) {
    const waiting = turn === 0 ? loop.arm("ses_1") : loop.turnEnded("ses_1", SUCCEEDED)
    loop.turnStarted("ses_1")
    await waiting
  }
  assert.equal(loop.stopped, null)
  assert.deepEqual(seen.reports, [])
})

test("a turn that ends before its cancelled wait returns is still waited after", async () => {
  let release: (result: HookResult) => void = () => {}
  let answers = 0
  const { loop, seen } = harness(() =>
    answers++ === 0 ? new Promise<HookResult>((resolve) => (release = resolve)) : Promise.resolve(BLOCK),
  )
  const waiting = loop.arm("ses_1")
  loop.turnStarted("ses_1")
  await loop.turnEnded("ses_1", SUCCEEDED)
  release({ code: null, out: "" })
  await waiting
  await new Promise((resolve) => setImmediate(resolve))
  assert.equal(seen.asked, 2)
  assert.deepEqual(seen.prompts, ["You have new messages"])
})

test("a turn that starts while the loop backs off is not waited through, and its end waits again", async () => {
  let wake: () => void = () => {}
  let asked = 0
  const loop = new Loop({
    agent: "reviewer",
    hook: async () => {
      asked += 1
      return BLOCK
    },
    prompt: async () => {},
    sleep: () => new Promise<void>((resolve) => (wake = resolve)),
    report: () => {},
    lastTurn: () => {},
    now: () => 0,
  })
  const failed = loop.turnEnded("ses_1", TURN_FAILED, { message: "down" })
  loop.turnStarted("ses_1")
  wake()
  await failed
  assert.equal(asked, 0, "not asked during the turn")

  await loop.turnEnded("ses_1", SUCCEEDED)
  assert.equal(asked, 1)
})

test("a turn starting with no wait blocked is the plugin's own prompt and cancels nothing", async () => {
  const { loop, seen } = harness([BLOCK, BLOCK])
  await loop.arm("ses_1")
  assert.equal(loop.turnStarted("ses_1"), false)
  await loop.turnEnded("ses_1", SUCCEEDED)
  assert.deepEqual(seen.prompts, Array(2).fill("You have new messages"))
})

test("a failed turn is followed by a wait before the hook is asked", async () => {
  const { loop, seen } = harness([BLOCK])
  await loop.turnEnded("ses_1", TURN_FAILED)
  assert.deepEqual(seen.sleeps, [FAILURE_BACKOFF_MS])
  assert.deepEqual(seen.prompts, ["You have new messages"])
})

test("turns that drain nothing stop the loop, which says so", async () => {
  const { loop, seen } = harness(Array(MAX_WASTED_TURNS + 1).fill(BLOCK))
  // Arming closes no turn, so it is not one of the wasted ones.
  await loop.arm("ses_1")
  for (let turn = 0; turn < MAX_WASTED_TURNS; turn++) await loop.turnEnded("ses_1", SUCCEEDED)

  assert.equal(seen.reports.length, 1)
  assert.match(seen.reports[0], /^5 consecutive turns were prompted for unread messages and read none/)
  assert.equal(seen.prompts.length, MAX_WASTED_TURNS + 1)
  assert.equal(
    seen.prompts.at(-1),
    `pm: never-idle loop stopped: ${seen.reports[0]}. This agent no longer wakes for messages; ` +
      "restart it with `pm agent restart reviewer`.",
  )
  assert.equal(loop.stopped, seen.reports[0])

  await loop.turnEnded("ses_1", SUCCEEDED)
  assert.equal(seen.asked, MAX_WASTED_TURNS + 1, "a stopped loop asked the hook again")
})

test("turns that fail stop the loop with the error of the last one", async () => {
  const { loop, seen } = harness(Array(MAX_WASTED_TURNS + 1).fill(BLOCK))
  await loop.arm("ses_1")
  const unavailable = { type: "provider.no-route", message: "Model unavailable: local/nope" }
  await loop.turnEnded("ses_1", TURN_FAILED, { type: "provider.invalid-request", message: "earlier", status: 404 })
  for (let turn = 1; turn < MAX_WASTED_TURNS; turn++) await loop.turnEnded("ses_1", TURN_FAILED, unavailable)

  assert.deepEqual(seen.reports, [
    `${MAX_WASTED_TURNS} consecutive turns were prompted for unread messages and read none; ` +
      "the last one failed: Model unavailable: local/nope (provider.no-route)",
  ])
})

test("a failure is not carried past the turn it ended", async () => {
  const { loop, seen } = harness(Array(MAX_WASTED_TURNS + 1).fill(BLOCK))
  await loop.arm("ses_1")
  await loop.turnEnded("ses_1", TURN_FAILED, { type: "provider.no-route", message: "Model unavailable: x" })
  for (let turn = 1; turn < MAX_WASTED_TURNS; turn++) await loop.turnEnded("ses_1", SUCCEEDED)
  assert.equal(seen.reports.length, 1)
  assert.doesNotMatch(seen.reports[0], /Model unavailable/)
})

test("a failed turn's error is recorded as it ends, and cleared by a turn that succeeds", async () => {
  const { loop, seen } = harness([BLOCK, BLOCK, BLOCK, BLOCK])
  await loop.turnEnded("ses_1", TURN_FAILED, { type: "provider.no-route", message: "Model unavailable: x" })
  assert.deepEqual(seen.lastTurn, ["Model unavailable: x (provider.no-route)"])
  // An interrupted turn neither failed nor succeeded.
  await loop.turnEnded("ses_1", "session.execution.interrupted")
  assert.deepEqual(seen.lastTurn, ["Model unavailable: x (provider.no-route)"])
  await loop.turnEnded("ses_1", SUCCEEDED)
  assert.deepEqual(seen.lastTurn, ["Model unavailable: x (provider.no-route)", null])
})

test("a failed turn's error is recorded before the back-off, not after it", async () => {
  const recorded: (string | null)[] = []
  let release = () => {}
  const waiting = new Loop({
    agent: "reviewer",
    hook: async () => BLOCK,
    prompt: async () => {},
    sleep: () => new Promise<void>((resolve) => (release = resolve)),
    report: () => {},
    lastTurn: (error) => recorded.push(error),
  }).turnEnded("ses_1", TURN_FAILED, { message: "down" })
  assert.deepEqual(recorded, ["down"])
  release()
  await waiting
})

test("a turn's error reads as its message, kind and status", () => {
  assert.equal(
    turnError({ type: "provider.invalid-request", message: "The model `q` does not exist.", status: 404 }),
    "The model `q` does not exist. (provider.invalid-request, HTTP 404)",
  )
  assert.equal(turnError({ type: "provider.no-route", message: "Model unavailable: local/x" }), "Model unavailable: local/x (provider.no-route)")
  assert.equal(turnError(undefined), "no error reported")
})

test("a hook that fails is asked again after a wait, then the loop carries on", async () => {
  const { loop, seen } = harness([{ code: 1, out: "" }, { code: 0, out: "{}" }, BLOCK])
  await loop.arm("ses_1")
  assert.deepEqual(seen.sleeps, RETRY_MS.slice(0, 2))
  assert.deepEqual(seen.prompts, ["You have new messages"])
  assert.deepEqual(seen.reports, [])
})

test("a `pm` that keeps failing stops the loop, which says so", async (t) => {
  const children = new Set<any>()
  const command = fakePm(t, "echo 'error: not inside a pm project' >&2; exit 1")
  const { loop, seen } = harness(() =>
    runPm(["harness", "hooks", "stop"], "{}", { cwd: tmpdir(), env: process.env, children, command }),
  )
  await loop.arm("ses_1")

  assert.equal(seen.asked, MAX_FAILURES)
  assert.deepEqual(seen.sleeps, RETRY_MS)
  assert.deepEqual(seen.reports, [`\`pm harness hooks stop\` failed ${MAX_FAILURES} times in a row (exit 1)`])
  assert.equal(seen.prompts.length, 1)
  assert.match(seen.prompts[0], /^pm: never-idle loop stopped: `pm harness hooks stop` failed/)
  assert.equal(children.size, 0)
})

test("a `pm` that cannot be started is a failure, not an empty answer", async () => {
  const children = new Set<any>()
  const result = await runPm(["harness", "hooks", "stop"], "{}", {
    cwd: tmpdir(),
    env: process.env,
    children,
    command: join(tmpdir(), "no-such-pm-binary"),
  })
  assert.equal(result.code, -1)
  assert.deepEqual(hookDecision(result), { failure: "exit -1" })
})

test("`pm` is given the hook's input and its answer is returned", async (t) => {
  const command = fakePm(t, 'printf \'{"decision":"block","reason":"%s %s"}\' "$*" "$(cat)"')
  const result = await runPm(["harness", "hooks", "stop"], "{}", {
    cwd: tmpdir(),
    env: process.env,
    children: new Set(),
    command,
  })
  assert.deepEqual(hookDecision(result), { block: "harness hooks stop {}" })
})

test("a cancelled `pm` is killed with a signal it cannot catch", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "pm-plugin-test-"))
  t.after(() => rmSync(dir, { recursive: true, force: true }))
  const caught = join(dir, "caught")
  const ready = join(dir, "ready")
  const command = fakePm(t, `trap 'touch ${caught}; exit 0' TERM INT; touch ${ready}; while :; do sleep 0.05; done`)
  const cancel = new AbortController()
  const children = new Set<any>()
  const running = runPm(["harness", "hooks", "stop"], "{}", {
    cwd: tmpdir(),
    env: process.env,
    children,
    command,
    signal: cancel.signal,
  })
  while (!existsSync(ready)) await new Promise((resolve) => setTimeout(resolve, 10))
  cancel.abort()
  const result = await running
  assert.equal(result.code, null)
  assert.equal(existsSync(caught), false)
  assert.equal(children.size, 0)
})

test("a subscription that keeps ending stops the loop; one that recovers does not", async () => {
  const { loop, seen } = harness([])
  for (let attempt = 1; attempt < MAX_FAILURES - 1; attempt++) {
    assert.equal(await loop.subscriptionEnded("ses_1", "the stream closed"), true)
  }
  loop.eventReceived()
  for (let attempt = 1; attempt < MAX_FAILURES; attempt++) {
    assert.equal(await loop.subscriptionEnded("ses_1", "the stream closed"), true, `attempt ${attempt}`)
  }
  assert.deepEqual(seen.reports, [])

  assert.equal(await loop.subscriptionEnded("ses_1", "the stream closed"), false)
  assert.deepEqual(seen.reports, [
    `its event subscription ended ${MAX_FAILURES} times in a row (the stream closed)`,
  ])
  assert.equal(seen.prompts.length, 1)
})

test("an unloaded loop neither asks nor prompts", async () => {
  const { loop, seen } = harness([])
  loop.unload()
  await loop.arm("ses_1")
  await loop.turnEnded("ses_1", SUCCEEDED)
  assert.equal(await loop.subscriptionEnded("ses_1", "aborted"), false)
  assert.deepEqual(seen, { prompts: [], sleeps: [], reports: [], lastTurn: [], asked: 0 })
})

test("a permission ask or any form is a dialog waiting on the user", () => {
  assert.deepEqual(
    askOf({
      type: "permission.asked",
      data: { id: "per_1", sessionID: "s1", action: "edit", resources: ["src/a.ts"], source: { type: "tool" } },
    }),
    { id: "per_1", sessionID: "s1", payload: { hook_event_name: "PermissionRequest", detail: "edit src/a.ts" } },
  )
  const form = (metadata: object, title: string, sessionID = "s1") => ({
    type: "form.created",
    data: {
      form: {
        id: "frm_1",
        sessionID,
        title,
        metadata,
        fields: [{ key: "q0", title: "Database", description: "Which DB?", type: "string" }],
      },
    },
  })
  assert.deepEqual(askOf(form({ kind: "question" }, "Questions")), {
    id: "frm_1",
    sessionID: "s1",
    payload: { hook_event_name: "Question", detail: "Which DB?" },
  })
  assert.deepEqual(askOf(form({ kind: "websearch.provider" }, "Web Search")), {
    id: "frm_1",
    sessionID: "s1",
    payload: { hook_event_name: "Dialog", detail: "Web Search: Which DB?" },
  })
  const elicitation = { kind: "mcp-elicitation", server: "docs", message: "Pick a space" }
  assert.deepEqual(askOf(form(elicitation, "docs is requesting input", "global")), {
    id: "frm_1",
    sessionID: null,
    payload: { hook_event_name: "Dialog", detail: "docs is requesting input: Pick a space" },
  })
  assert.equal(askOf({ type: "session.execution.succeeded", data: { sessionID: "s1" } }), null)
})

test("a failed turn reports its error, any other turn end nothing", () => {
  assert.deepEqual(failedTurnOf(TURN_FAILED, { type: "provider.unavailable", message: "Model unavailable" }), {
    hook_event_name: "TurnFailed",
    detail: "Model unavailable (provider.unavailable)",
  })
  assert.equal(failedTurnOf(SUCCEEDED, undefined), null)
  assert.equal(failedTurnOf("session.execution.interrupted", undefined), null)
})

test("a reply, an answer or a dismissal names the dialog it closes", () => {
  assert.equal(answeredOf({ type: "permission.replied", data: { sessionID: "s1", requestID: "per_1", reply: "reject" } }), "per_1")
  assert.equal(answeredOf({ type: "form.replied", data: { id: "frm_1", sessionID: "s1", answer: {} } }), "frm_1")
  assert.equal(answeredOf({ type: "form.cancelled", data: { id: "frm_1", sessionID: "s1" } }), "frm_1")
  assert.equal(answeredOf({ type: "form.created", data: { form: { id: "frm_1" } } }), null)
})

test("the agent stops waiting only once every open dialog has closed", () => {
  const asks = new Asks()
  asks.opened("per_1")
  asks.opened("frm_1")
  assert.equal(asks.closed("per_1"), false)
  assert.equal(asks.closed("unknown"), false)
  assert.equal(asks.closed("frm_1"), true)
  assert.equal(asks.closed("frm_1"), false, "nothing was open")
})

const ASK = { id: "per_1", sessionID: "ses_1", action: "edit", resources: ["src/a.rs"] }

/** Remote asks over a scripted dialog hook, recording what was replied. */
function remoteAsks(hook: (payload: string, cancel: AbortSignal) => Promise<HookResult>, reply?: () => Promise<unknown>) {
  const replies: object[] = []
  const remote = new RemoteAsks({
    hook,
    reply: async (r) => {
      replies.push(r)
      await reply?.()
    },
  })
  return { remote, replies }
}

test("a permission ask is replied to with the decision the dialog hook printed", async () => {
  const payloads: string[] = []
  const { remote, replies } = remoteAsks(async (payload) => {
    payloads.push(payload)
    return { code: 0, out: '{"decision":"reject","message":"not that file"}' }
  })
  await remote.asked(ASK)
  assert.deepEqual(payloads.map((p) => JSON.parse(p)), [ASK])
  assert.deepEqual(replies, [{ sessionID: "ses_1", requestID: "per_1", decision: "reject", message: "not that file" }])
})

test("an ask settled at the TUI ends its dialog hook and sends no reply", async () => {
  let ended = false
  const { remote, replies } = remoteAsks(
    (_payload, cancel) =>
      new Promise((resolve) =>
        cancel.addEventListener("abort", () => {
          ended = true
          resolve({ code: 0, out: '{"decision":"once"}' })
        }),
      ),
  )
  const asked = remote.asked(ASK)
  remote.replied("per_2")
  assert.equal(ended, false, "another ask's reply")
  remote.replied("per_1")
  await asked
  assert.equal(ended, true)
  assert.deepEqual(replies, [])
})

test("a hook that prints no decision sends no reply, and a refused reply is not an error", async () => {
  for (const out of ["", "{}", '{"decision":"maybe"}']) {
    const { remote, replies } = remoteAsks(async () => ({ code: 0, out }))
    await remote.asked(ASK)
    assert.deepEqual(replies, [], out)
  }
  const { remote, replies } = remoteAsks(
    async () => ({ code: 0, out: '{"decision":"once"}' }),
    async () => {
      throw new Error("Permission request not found")
    },
  )
  await remote.asked(ASK)
  assert.equal(replies.length, 1)
})

test("a cancelled `pm` given a kill signal gets that one", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "pm-plugin-test-"))
  t.after(() => rmSync(dir, { recursive: true, force: true }))
  const caught = join(dir, "caught")
  const ready = join(dir, "ready")
  const command = fakePm(t, `trap 'touch ${caught}; exit 0' TERM; touch ${ready}; while :; do sleep 0.05; done`)
  const cancel = new AbortController()
  const running = runPm(["harness", "hooks", "dialog", "opencode"], "{}", {
    cwd: tmpdir(),
    env: process.env,
    children: new Set(),
    command,
    signal: cancel.signal,
    killSignal: "SIGTERM",
  })
  while (!existsSync(ready)) await new Promise((resolve) => setTimeout(resolve, 10))
  cancel.abort()
  await running
  assert.equal(existsSync(caught), true)
})
