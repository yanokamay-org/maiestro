# Worktree theming

How a spawned worktree gets its color and emoji, and how that one color reaches the popover row, the VS Code window, and the Claude session. The code lives in `backend/src/theming.rs` (`pick_theme`, `claude_color`) and `backend/src/editor.rs` (`write_vscode_files`).

A spawned worktree gets a deterministic color and emoji from `theming::pick_theme`, seeded by the workspace name and avoiding colors already claimed by tracked sessions. That one color is applied in three places so the visual thread survives from the dashboard to where the user actually works: the popover row, VS Code's title/status/activity bars (`editor::write_vscode_files`), and the Claude session's own UI.

`PALETTE` **is** Claude Code's session palette: the eight entries are the exact hex values Claude Code's standard theme paints its eight session colors with (`red, blue, green, yellow, purple, orange, pink, cyan`), in the order `/color` lists them. Using Claude's own values, rather than a hand-darkened approximation, is what makes the row, the bars, and the session read as one color (issue #142: the old dark palette's "orange" read as brown next to Claude's terracotta). `theming::claude_color` is therefore a **bijection**: no two live sessions collide on a session color while a distinct one goes unused. Two unit tests enforce this — one for the bijection (a ninth palette entry fails the build rather than silently doubling up) and one pinning each hex to Claude's RGB value (a drifted hex fails rather than shipping a mismatched title bar).

| Claude color | Hex | Emoji |
|---|---|---|
| red | `#dc2626` | 🔴 🍒 🌶️ 🦞 🍓 |
| blue | `#6a9bcc` | 🔵 🫐 🦋 💙 🐳 |
| green | `#16a34a` | 🟢 🌿 🐸 🍀 🐊 |
| yellow | `#ca8a04` | 🟡 🍋 🌻 🐝 ⭐ |
| purple | `#827dbd` | 🟣 🔮 💜 🍇 |
| orange | `#d97757` | 🟠 🦊 🍊 🦁 🍑 |
| pink | `#c46686` | 🎀 🌸 🦩 🐷 🩷 |
| cyan | `#0891b2` | 🐬 🐠 🌊 🧊 🦚 |

Where the values come from: Claude Code ships as a single binary with its themes embedded, and each theme carries the session colors as `<name>_FOR_SUBAGENTS_ONLY` entries. To re-check them after a Claude Code upgrade:

```
strings -n 6 "$(readlink -f "$(which claude)")" | grep -o -E '(red|blue|green|yellow|purple|orange|pink|cyan)_FOR_SUBAGENTS_ONLY:"[^"]+"'
```

The `rgb(...)` blocks are the standard dark and light themes (they share one set of values, which is what the palette pins); the `ansi:` blocks are the ANSI themes and the remaining `rgb` blocks the daltonized ones, none of which we track.

`claude_color` is keyed off each session's **stored** hex and also maps every retired palette hex (the pre-#142 dark palette, and the dark cyan that palette had itself replaced), so a session recorded under an older palette keeps the color it was given. `pick_theme` likewise avoids colors by their *Claude color* rather than raw hex, so an old-hex session still claims its session color against new spawns.

Because the palette is mid-tone rather than dark, `write_vscode_files` also pins a white foreground on the title, status, and activity bars: VS Code does not auto-contrast, and its theme default (light grey on dark themes, near-black on light ones) is unreadable on several of these backgrounds.

The color reaches the session as a **`/color <name>` initial prompt** appended to the launch command:

```
'/opt/homebrew/bin/claude' --remote-control --name '🎀 #127 — Add session color' '/color pink'
```

The binary is the **resolved** `claude` path (`tools::resolve_tool`), not a bare `claude` left to PATH. The task runs in VS Code's integrated terminal, whose PATH is whatever that VS Code process inherited — and a VS Code launched by a packaged mAIestro Code at login can carry the minimal Launch Services PATH, with no `claude` on it. So the same `tool_paths.claude` override that pins mAIestro Code's own drafting calls also decides which binary the *session* starts with; when nothing concrete resolves, `resolve_tool` yields the bare name, leaving it to PATH. Existing worktrees pick the resolved path up on their next reopen, via the `refresh_vscode_files` regeneration described below.

**Codex sessions get no session color.** For a repo whose agentic coding CLI is Codex (issue #162), the task runs the resolved `codex` binary alone — `'/opt/homebrew/bin/codex'`, labelled "Start Codex". Codex has no `--name` or `/color`, and any initial prompt would start a real model turn, so the session itself carries neither the name nor the color. Everything else still applies: the popover row and VS Code's title/status/activity bars are themed with the worktree's color exactly as for Claude, and `window.title`, the terminal font, and teardown's window matching don't depend on the agent. The session record stores the agent, so the regenerated task on reopen keeps launching the record's agent: the one the worktree was spawned with, unless the user switched it from the row's command strip (see *Switching a session's agentic coding CLI* below).

**Antigravity sessions get no session color either.** For a repo whose agentic coding CLI is Antigravity (issue #185), the task runs the resolved `agy` binary alone, labelled "Start Antigravity". `agy` has no session-name flag and no `/color`, and an initial prompt (`-i`) would start a real model turn, so the same rules as Codex apply. The bars, popover row and window title are still themed. Its status hooks live in the worktree's `.agents/hooks.json`, not on the command line (`docs/session-status.md`).

Not via a flag: Claude Code's `--agent-color` is only honored alongside `--agent-id`/`--agent-name`/`--team-name` (teammate sessions) and is **silently ignored** on its own. A leading-slash initial prompt is dispatched as a command, so this costs one line in the transcript and no model call.

Because the launch command lives in the worktree's generated `.vscode/tasks.json`, a worktree keeps whatever its original spawn baked in. So the **reuse** spawn path regenerates the `.vscode` files (`spawn::refresh_vscode_files`), mirroring what `reconcile_session_hooks` does for hooks: reopening picks up changes to what we generate, so a worktree spawned by an older build is brought up to date on its next reopen. The values come from the **session record**, not the caller's freshly picked theme — reopening must never re-theme a worktree. It is best-effort: no session record means no rewrite, and a write failure only warns. Note this overwrites `.vscode/settings.json` and `tasks.json` on every reopen, so hand edits to those generated files do not survive.

## Switching a session's agentic coding CLI

**Agentic Coding CLI…** in a row's command strip opens a picker listing the available agentic coding CLIs (`AgentSwitchDialog`, current one marked, with a short explanation). Choosing another (issue #186, `session_agent::session_set_agent`) changes an existing worktree's agentic coding CLI without re-spawning it. It updates the session record's `agent` and installs the new agent's status hooks (removing our Claude hooks from `.claude/settings.local.json` when leaving Claude, or our `maiestro-status` group from `.agents/hooks.json` when leaving Antigravity, and keeping the user's). It then regenerates the `.vscode` files from the record with `refresh_vscode_files`, so the switch never re-themes the worktree: the color, emoji, title, and branch stay the same. The old agent's conversation does not carry over.

The folder-open task only runs when VS Code opens the folder fresh. So if the worktree's window is open during a switch, the record also stores `editor_agent`, the agent that window is still running, and the row offers **Restart VS Code**. `session_restart_editor` closes the window and confirms it is gone, reusing teardown's `editor::close_window_and_wait` and blocking with the same Accessibility shortcut when it can't. It then clears the status record and reopens the folder. If the user picks **Later**, `session_open_in_editor` returns `restart_required` instead of focusing the old agent's window, for as long as that window stays open. Once it is closed, the next open launches the new agent and clears `editor_agent`. Switching back before restarting cancels the pending restart.
