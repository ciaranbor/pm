# tmux integration

pm's tmux plugin is one line in your tmux config, after any `@pm-*` options
and any `bind s` or `bind w` of yours, since init reads them as they stand
when it runs:

```tmux
run-shell 'pm tmux init'
```

The plugin is part of the binary, so it always matches the installed pm.
tmux runs it with the server's environment, not your shell's, so `pm` must
be on the `PATH` the server started with; set `@pm-bin` to its full path
otherwise. Init only adds to your config and is safe to re-run on a reload.
It makes prefix `s` / `w` pm's tree, puts an agent badge in each window's
status entry, starts `status-right` with the summary of what needs you
(each attention kind's glyph, count and name), shows pm's announcements in
the middle of the status line, and keeps pm's state current on the server.

## Options

Set before `run-shell 'pm tmux init'`:

| Option | Default | Effect |
|---|---|---|
| `@pm-bin` | `pm` | the pm binary tmux runs |
| `@pm-auto-refresh` | on | keep pm's options current with a background `pm tmux refresh` loop; pm pushes its own changes at once, so the loop only catches what happens outside pm. The loop also re-sets pm's formats when it starts or pm is upgraded, so a new pm reaches a running server without a config reload; it switches to another pm when `@pm-bin` or the server's `PATH` comes to name one |
| `@pm-refresh-interval` | `30` | seconds between refreshes |
| `@pm-window-status` | on | put each agent window's badge just before the window name in `window-status-format` and `window-status-current-format`, keeping your theme's style for the name |
| `@pm-status-right` | on | start `status-right` with `@pm_summary`, unless it already names it. `status-right-length` cuts the right end, so on a long line your theme's last items go first; raise the length or turn this off |
| `@pm-status-format` | on | end `status-format[0]` with pm's announcement, centred between the window list and `status-right`, unless it already names `@pm_announcement`. With `status-justify centre` it sits beside the window list; with `status off` nothing shows it |
| `@pm-bind-tree` | on | turn prefix `s` / `w` into pm's tree, sorted by name, when they run tmux's default `choose-tree` |
| `@pm-attention-key` | `a` | the prefix key opening pm's tree with only the sessions needing attention, or a message when none does; `off` for none. A key your config binds is left alone; a key pm lets go of gets tmux's default binding back, if it has one |

To place the summary or the announcements yourself, turn init's placement
off (`@pm-status-right`, `@pm-status-format`) or name the option in your
own format, which init then leaves alone:

```tmux
set -g status-right '#{?@pm_summary,#{E:@pm_summary} ,}%H:%M'
set -ag 'status-format[0]' '#[nolist align=centre norange default]#{?@pm_announcement_hidden,#{@pm_announcement_window},#{@pm_announcement}}'
```

With `@pm-bind-tree off`, or to put the tree on another key:

```tmux
bind T choose-tree -Zs -O name -F '#{E:@pm_tree_format}' "run-shell \"pm tmux jump --client '#{client_name}' '%%'\""
```

## Badges

Badges are Nerd Font glyphs: an agent window's shows its [agent
state](remote-api.md#agent-states-and-activity), a feature session's the
attention it needs. A kind that means what a state means shares its glyph.
The window list shows glyphs only; pm's tree, which has room, labels each
one: a session line reads `<glyph> blocked  implementer: which DB?` (on
`main`, its main agent's state unless `main` is blocked; an asking `main`
keeps its agent's badge), a window line `<glyph> idle <envelope> 2`,
and a session's activity `<gear> working`, `<spinner> background 1d` or
`quiet 2h`. Follow a badge with a space in your own formats: some terminals
(Ghostty) draw a glyph small when the next cell isn't blank.

| Glyph | Colour | Agent state | Attention |
|---|---|---|---|
| `nf-fa-hand` | red | | `blocked` |
| `nf-fa-question_circle` | red | `asking` | `asking` |
| `nf-md-broom` | grey | | `cleanup` |
| `nf-fa-check_circle` | green | | `ready` |
| `nf-md-skull` | red | `dead` | `dead` |
| `nf-fa-bell_slash` | magenta | `unarmed` | `unarmed` |
| `nf-fa-pause` | yellow | | `stalled` |
| `nf-fa-gear` | green | `busy` | |
| `nf-fa-spinner` | green | `background` | |
| `nf-fa-hourglass_half` | grey | `idle` | |
| `nf-fa-stop` | grey | `stopped`, `closed` | |
| `nf-fa-envelope` | yellow | after the state: unread messages | |

## Published options

`pm tmux refresh` publishes every project's attention view as user options
for your own status line or formats. An option whose value goes away is
unset, text is escaped for formats, and each name is set at one scope only:

| Scope | Option | Value |
|---|---|---|
| feature or main session | `@pm_project`, `@pm_feature` | names; a `main` session has no `@pm_feature` |
| | `@pm_progress` | `wip`, `blocked` or `ready`; `main` is never `ready` |
| | `@pm_attention` | the attention kind; unset for `none` |
| | `@pm_reason` | the attention detail, or for `stalled` what the attention view shows; unset without one |
| | `@pm_badge` | the kind's glyph, styled; unset for `none`; on `main`, its main agent's badge unless `main` is blocked |
| | `@pm_label` | `@pm_badge` with words, as pm's tree shows it: the kind after its glyph; on `main`, its main agent's `@pm_agent_label` unless `main` is blocked |
| | `@pm_activity` | the busy glyph while the scope is working, else the background glyph and how long its oldest background wait has run (`1d`), else how long it has been quiet (`2h`, styled); unset under 10 minutes quiet, and on `main` while its badge is its main agent's and shows it busy |
| | `@pm_activity_label` | `@pm_activity` with words, as pm's tree shows it: `working` or `background 1d` after the glyph, or `quiet 2h`; unset when it is |
| | `@pm_alerted` | pm's own bookkeeping: the kinds already alerted on |
| agent window | `@pm_agent` | the agent's name |
| | `@pm_agent_state` | an [agent state](remote-api.md#agent-states-and-activity) |
| | `@pm_unread` | unread message count |
| | `@pm_agent_badge` | the badge, styled; it resets with `#[default]`, so placed anywhere but the start of a format, follow it with your theme's style |
| | `@pm_agent_label` | the badge with words, as pm's tree shows it: the state after its glyph, the unread count after the envelope |
| | `@pm_announcement_hidden`, `@pm_announcement_window` | pm's own bookkeeping: the window of an agent whose ask is announced, and the announcement without that ask, shown there instead |
| global | `@pm_summary` | each kind's glyph, how many sessions have `@pm_attention`, and the kind (`2 blocked`), styled and joined by ` · `; unset when none has |
| | `@pm_announcement` | the latest announcement (`pm: app/login ready: …`), escaped for formats; unset `display-time` after it is made (tmux's 750 ms when `display-time` is 0). Show it with `#{@pm_announcement}`, never `#{E:…}`, so a `%` in it survives |
| | `@pm_announcement_id` | pm's own bookkeeping: which announcement is up, so an older one's expiry leaves a newer one |
| | `@pm_count` | sessions with `@pm_attention` set, so it matches what `@pm-attention-key` opens; a feature whose session is closed is not counted (`pm status` lists it) |
| | `@pm_features_alerted` | pm's own bookkeeping: the kinds each feature has alerted on, kept for a closed feature |
| | `@pm_tree_format` | pm's `choose-tree` line format, set by `pm tmux init` |

## Another tmux server

Setting `PM_TMUX_SERVER=<name>` makes every `pm` command target that tmux
server (`tmux -L <name>`) instead of the default one; `pm open` run from a
pane of another server attaches a nested client rather than switching that
server's. `scripts/sandbox` uses it for a throwaway pm environment
(`scripts/sandbox --help`).
