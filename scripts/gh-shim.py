#!/usr/bin/env python3
"""The sandbox's `gh`: a fake GitHub kept as files under $HOME/gh, so PR
commands run offline. Every call is appended to $HOME/log/gh.argv.

  $HOME/gh/repo.json      `gh repo view --json` (edit to disallow methods
                          or change viewerDefaultMergeMethod)
  $HOME/gh/pr-<n>.json    PR <n> as `gh pr view --json` returns it; edit
                          state, isDraft, mergeable, statusCheckRollup, ...
                          (`gh pr checks` reads the rollup and never waits;
                          an open PR's headRefOid follows origin's branch;
                          `gh run view` names the open PRs' failing checks)
  $HOME/gh/pr-<n>.merge   optional: "queue" makes `gh pr merge` succeed
                          without merging; "fail" merges, then fails as gh
                          does in a linked worktree (cli/cli#13955)

`gh pr merge` lands the PR on the cwd repo's `origin`: the head branch is
merged (squash, merge or rebase) into the base there, and the PR marked
MERGED. Anything else exits 1 naming the unsupported call.
"""

import json
import os
import subprocess
import sys
import tempfile

home = os.environ["HOME"]
store = os.path.join(home, "gh")
os.makedirs(store, exist_ok=True)
os.makedirs(os.path.join(home, "log"), exist_ok=True)
args = sys.argv[1:]
with open(os.path.join(home, "log", "gh.argv"), "a") as log:
    log.write(" ".join(args) + "\n")


def git(*a, cwd=None):
    return subprocess.run(
        ["git", *a], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.strip()


def flag(name):
    return args[args.index(name) + 1] if name in args else None


def pr_path(n):
    return os.path.join(store, f"pr-{n}.json")


def load(n):
    try:
        with open(pr_path(n)) as f:
            return json.load(f)
    except FileNotFoundError:
        sys.exit(f"GraphQL: Could not resolve to a PullRequest with the number of {n}.")


def save(n, pr):
    with open(pr_path(n), "w") as f:
        json.dump(pr, f, indent=2)


def fields(obj):
    wanted = flag("--json")
    return {k: obj.get(k) for k in wanted.split(",")} if wanted else obj


def prs():
    for name in sorted(os.listdir(store)):
        if name.startswith("pr-") and name.endswith(".json"):
            yield name[3:-5], load(name[3:-5])


def pr_arg():
    """The PR a `gh pr <cmd> [<n>]` call names, else the current branch's."""
    if len(args) > 2 and not args[2].startswith("-"):
        return args[2]
    branch = git("rev-parse", "--abbrev-ref", "HEAD")
    for n, pr in prs():
        if pr["headRefName"] == branch and pr["state"] == "OPEN":
            return n
    sys.exit(f'no pull requests found for branch "{branch}"')


def follow_head(n, pr):
    """An open PR's head moves with its branch on origin, as on GitHub."""
    if pr["state"] == "OPEN":
        out = subprocess.run(
            ["git", "ls-remote", "origin", "refs/heads/" + pr["headRefName"]],
            capture_output=True, text=True,
        ).stdout.split()
        if out and out[0] != pr["headRefOid"]:
            pr["headRefOid"] = out[0]
            save(n, pr)
    return pr


def bucket(check):
    value = (check.get("conclusion") or check.get("state") or "").upper()
    if value in ("FAILURE", "ERROR", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED", "STARTUP_FAILURE"):
        return "fail"
    status = (check.get("status") or "").upper()
    if (status and status != "COMPLETED") or value in ("PENDING", "EXPECTED"):
        return "pending"
    return "pass"


def land(pr, method):
    origin = git("remote", "get-url", "origin")
    with tempfile.TemporaryDirectory() as tmp:
        git("clone", "-q", origin, tmp)
        git("checkout", "-q", pr["baseRefName"], cwd=tmp)
        head = "origin/" + pr["headRefName"]
        if method == "--squash":
            git("merge", "--squash", head, cwd=tmp)
            git("commit", "-qm", f"{pr['title']} (#{pr['number']})", cwd=tmp)
        elif method == "--rebase":
            git("rebase", "-q", pr["baseRefName"], head, cwd=tmp)
            git("checkout", "-q", "-B", pr["baseRefName"], cwd=tmp)
        else:
            git("merge", "-q", "--no-ff", "-m", f"Merge pull request #{pr['number']}", head, cwd=tmp)
        git("push", "-q", "origin", pr["baseRefName"], cwd=tmp)


match args[:2]:
    case ["auth", "status"]:
        print("github.com: logged in to the sandbox's fake GitHub")
    case ["repo", "view"]:
        path = os.path.join(store, "repo.json")
        repo = {
            "mergeCommitAllowed": True,
            "squashMergeAllowed": True,
            "rebaseMergeAllowed": True,
            "viewerDefaultMergeMethod": "MERGE",
        }
        if os.path.exists(path):
            with open(path) as f:
                repo = json.load(f)
        else:
            with open(path, "w") as f:
                json.dump(repo, f, indent=2)
        print(json.dumps(fields(repo)))
    case ["pr", "list"]:
        head = flag("--head")
        found = [pr for _, pr in prs() if pr["headRefName"] == head and pr["state"] == "OPEN"]
        print(json.dumps([fields(pr) for pr in found[:1]]))
    case ["pr", "create"]:
        head, base = flag("--head"), flag("--base")
        n = str(1 + max((int(k) for k, _ in prs()), default=0))
        url = f"https://github.com/sandbox/repo/pull/{n}"
        save(n, {
            "number": int(n),
            "url": url,
            "title": git("log", "-1", "--format=%s", head),
            "body": flag("--body") or "",
            "state": "OPEN",
            "isDraft": "--draft" in args,
            "mergeable": "MERGEABLE",
            "reviewDecision": "",
            "headRefName": head,
            "headRefOid": git("rev-parse", head),
            "baseRefName": base,
            "statusCheckRollup": [],
        })
        print(url)
    case ["pr", "view"]:
        n = pr_arg()
        print(json.dumps(fields(follow_head(n, load(n)))))
    case ["pr", "checks"]:
        n = pr_arg()
        pr = follow_head(n, load(n))
        rollup = pr.get("statusCheckRollup") or []
        if not rollup:
            sys.exit(f"no checks reported on the '{pr['headRefName']}' branch")
        buckets = [bucket(c) for c in rollup]
        for check, b in zip(rollup, buckets):
            print(f"{check.get('name') or check.get('context')}\t{b}")
        sys.exit(1 if "fail" in buckets else 8 if "pending" in buckets else 0)
    case ["pr", "ready"]:
        for n, pr in prs():
            if args[2] in (n, pr["headRefName"]):
                pr["isDraft"] = False
                save(n, pr)
    case ["pr", "edit"]:
        pr = load(args[2])
        pr["title"] = flag("--title") or pr["title"]
        pr["body"] = flag("--body") or pr["body"]
        save(args[2], pr)
    case ["pr", "merge"]:
        n = args[2]
        pr = load(n)
        if pr["state"] != "OPEN":
            sys.exit(f"Pull request #{n} is not open")
        sha = flag("--match-head-commit")
        if sha and sha != pr["headRefOid"]:
            sys.exit(f"Head branch was modified. Review and try the merge again.")
        mode_path = os.path.join(store, f"pr-{n}.merge")
        mode = open(mode_path).read().strip() if os.path.exists(mode_path) else ""
        if mode == "queue":
            print(f"! Pull request #{n} will be automatically merged when all requirements are met")
            sys.exit(0)
        method = next(a for a in args if a in ("--merge", "--squash", "--rebase"))
        allowed = {
            "--merge": "mergeCommitAllowed",
            "--squash": "squashMergeAllowed",
            "--rebase": "rebaseMergeAllowed",
        }
        repo_path = os.path.join(store, "repo.json")
        if os.path.exists(repo_path) and not json.load(open(repo_path)).get(allowed[method], True):
            sys.exit(f"GraphQL: {method[2:]} merges are not allowed on this repository")
        land(pr, method)
        pr["state"] = "MERGED"
        save(n, pr)
        if mode == "fail":
            sys.exit(f"failed to run git: fatal: '{pr['baseRefName']}' is already used by worktree")
        print(f"✓ Merged pull request #{n}")
    case ["run", "view"]:
        failed = [
            (c.get("name") or c.get("context"))
            for _, pr in prs()
            if pr["state"] == "OPEN"
            for c in pr.get("statusCheckRollup") or []
            if bucket(c) == "fail"
        ]
        for name in failed:
            print(f"{name}\tsandbox shim: no real log; the check failed")
    case _:
        sys.exit(f"gh (sandbox shim): unsupported call: gh {' '.join(args)}")
