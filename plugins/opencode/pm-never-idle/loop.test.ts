import assert from "node:assert/strict"
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test, type TestContext } from "node:test"
import {
  Breaker,
  FAILURE_BACKOFF_MS,
  IMMEDIATE_MS,
  Loop,
  MAX_FAILURES,
  MAX_WASTED_TURNS,
  RETRY_MS,
  TURN_FAILED,
  consumedMessage,
  drivesSession,
  hookDecision,
  type HookResult,
} from "./loop.ts"
import { runPm } from "./pm.ts"

const FAST = IMMEDIATE_MS - 1
const SUCCEEDED = "session.execution.succeeded"
const BLOCK: HookResult = { code: 0, out: '{"decision":"block","reason":"You have new messages"}' }

/** A loop over scripted hook answers, recording what it did. */
function harness(answers: HookResult[] | (() => Promise<HookResult>)) {
  const seen = { prompts: [] as string[], sleeps: [] as number[], reports: [] as string[], asked: 0 }
  const queue = Array.isArray(answers) ? [...answers] : null
  const loop = new Loop({
    agent: "reviewer",
    hook: async () => {
      seen.asked += 1
      if (!queue) return (answers as () => Promise<HookResult>)()
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

test("only a block from a hook that exited cleanly is a decision", () => {
  assert.deepEqual(hookDecision(BLOCK), { block: "You have new messages" })
  assert.deepEqual(hookDecision({ code: 0, out: "{}" }), { failure: "it did not recognise this agent" })
  assert.deepEqual(hookDecision({ code: 0, out: '{"decision":"block"}' }), {
    failure: "it did not recognise this agent",
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
  assert.deepEqual(seen, { prompts: [], sleeps: [], reports: [], asked: 0 })
})
