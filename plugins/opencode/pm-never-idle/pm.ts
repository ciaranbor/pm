import { spawn, type ChildProcess } from "node:child_process"
import type { HookResult } from "./loop.ts"

export type PmOptions = {
  cwd: string
  env: NodeJS.ProcessEnv
  /** Every running child is kept here so an unload can kill it. */
  children: Set<ChildProcess>
  command?: string
  /** Kills the child with SIGKILL once aborted. */
  signal?: AbortSignal
}

/** Run `pm <args>` with `stdin`; a `pm` that cannot be started is code -1. */
export function runPm(args: string[], stdin: string, options: PmOptions): Promise<HookResult> {
  return new Promise((resolve) => {
    const child = spawn(options.command ?? "pm", args, {
      cwd: options.cwd,
      env: options.env,
      stdio: ["pipe", "pipe", "ignore"],
    })
    options.children.add(child)
    const kill = () => child.kill("SIGKILL")
    options.signal?.addEventListener("abort", kill, { once: true })
    child.on("exit", () => options.signal?.removeEventListener("abort", kill))
    let out = ""
    child.stdout!.on("data", (d) => (out += d))
    child.on("error", () => {
      options.children.delete(child)
      resolve({ code: -1, out })
    })
    child.on("close", (code) => {
      options.children.delete(child)
      resolve({ code, out })
    })
    child.stdin!.on("error", () => {})
    child.stdin!.end(stdin)
  })
}
