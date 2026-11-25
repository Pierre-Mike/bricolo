# Installing this skill for coding agents

This skill follows the open Agent Skills format: a folder named after the skill, holding `SKILL.md` with YAML front matter (`name`, `description`) and Markdown instructions, plus `references/` that the agent reads only when a step points there. Any agent that reads that format can use it, and an agent without skill support can follow `SKILL.md` as plain instructions.

## Install

The skill is compiled into `bricolo`, so an installed `bricolo` writes it without the repository:

```bash
bricolo skill install                          # --agent all: ~/.agents/skills and ~/.claude/skills
bricolo skill install --agent cursor,gemini    # each agent's own folder as well
bricolo skill install --dir .agents/skills     # a project's folder, shared through git
bricolo skill install --dry-run                # print the folders, write nothing
bricolo skill path                             # where it is installed, and which bricolo wrote it
bricolo skill snippet                          # a paragraph for AGENTS.md or GEMINI.md
```

`bricolo skill install` replaces an older copy of this skill, or a link to one, and refuses any other folder in its place. Start a new agent session afterwards; agents read skill folders at start-up. In the repository, `just install-skill` links both folders to `skills/create-bricolo-app`, so edits show at once.

## Where each agent looks

Checked on 2026-11-16 against each agent's documentation.

| Agent | User folder | Project folder | Also reads | Source |
|---|---|---|---|---|
| Claude Code | `~/.claude/skills/<name>/SKILL.md` (moves with `CLAUDE_CONFIG_DIR`) | `.claude/skills/` | plugin skills, `--add-dir` folders | [code.claude.com/docs/en/skills](https://code.claude.com/docs/en/skills) |
| Codex CLI | `~/.agents/skills` | `.agents/skills` in the current folder, its parent, and the repository root | `/etc/codex/skills`, built-in skills | [learn.chatgpt.com/docs/build-skills](https://learn.chatgpt.com/docs/build-skills.md) (developers.openai.com/codex/skills redirects there) |
| Gemini CLI | `~/.gemini/skills` | `.gemini/skills` | `~/.agents/skills` and `.agents/skills` as aliases, which win over `.gemini/skills` in the same tier | [geminicli.com/docs/cli/skills](https://geminicli.com/docs/cli/skills/) |
| OpenCode | `~/.config/opencode/skills` | `.opencode/skills`, searched up to the project root | `~/.claude/skills`, `~/.agents/skills`, `.claude/skills`, `.agents/skills` | [opencode.ai/docs/skills](https://opencode.ai/docs/skills) |
| Cursor | `~/.cursor/skills` | `.cursor/skills`, `.agents/skills`, also in subfolders | `~/.agents/skills`, `~/.claude/skills`, `~/.codex/skills` and their project forms | [cursor.com/docs/skills](https://cursor.com/docs/skills) |

The format itself: [agentskills.io/specification](https://agentskills.io/specification). `name` is 1 to 64 lowercase letters, digits and single hyphens, and matches the folder name. `description` is at most 1024 characters and says what the skill does and when to use it. The spec suggests keeping `SKILL.md` under 500 lines and about 5000 tokens, with references one level deep.

## What `--agent` writes

| `--agent` | Folder |
|---|---|
| `all` (default) | `~/.agents/skills/create-bricolo-app` and `~/.claude/skills/create-bricolo-app` |
| `agents`, `codex` | `~/.agents/skills/create-bricolo-app` |
| `claude` | `~/.claude/skills/create-bricolo-app` |
| `gemini` | `~/.gemini/skills/create-bricolo-app` |
| `opencode` | `~/.config/opencode/skills/create-bricolo-app` (`$XDG_CONFIG_HOME` if set) |
| `cursor` | `~/.cursor/skills/create-bricolo-app` |

`all` writes two folders because Claude Code documents only `~/.claude/skills`, and every other agent above reads `~/.agents/skills`. OpenCode and Cursor read both, so they may list the skill twice; the copies are the same. `--link` makes the second folder a link to the first.

## Agents without a skills folder

For an agent that reads only an instructions file (`AGENTS.md`, `GEMINI.md`, `CLAUDE.md`, `.cursorrules` and similar), install the skill, then add the output of `bricolo skill snippet` to that file. It tells the agent to read `SKILL.md` from the installed folder when the user asks for a bricolo app.

## Writing the skill so every agent can use it

- Instructions name actions ("run this command", "read this file"), never one agent's tool names.
- Commands are `bricolo`, `cargo`, `just`, and standard shell (`curl`, `lsof`, `python3`, `node`).
- `SKILL.md` stays under 250 lines; details live in `references/`, linked from the step that needs them.
- Optional front matter (`license`, `compatibility`, `metadata`) is in the spec; `allowed-tools` is not used, because tool names differ between agents.
