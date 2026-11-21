# Changelog

Each release has a section headed `## <version>`. The release script copies that section into the update notes, so write it for the people who will read it.

## 0.4.5 — 2026-11-21

- Tasks have tags. Important and On hold have one-click toggles in the drawer, and the board filters by tag.
- Tasks can suggest a model for the agent that picks them up. The suggestion is advice only.
- Project docs include HTML pages an agent generated. They open in a sandbox with no scripts and no outside requests.
- Tasks can be blocked by, or relate to, other tasks. Subtasks show their parent. A task that cannot be picked up on its own says why. Agents see blocked tasks.
- Comments on tasks, from people and agents, with the author and model recorded.
- Task descriptions render Markdown, including code blocks. Related links on a task open in your browser.
- Dashboard: the app opens on it. It shows totals, each board, upcoming work and recent activity across all boards. Each board has its own dashboard, opened from its name.
- Sprints: each board has sprints with a default length, a manual length or end date, and one running sprint at a time. Tasks join a sprint, and the board filters by sprint.
- Saved views: a board or list with filters baked in, chosen from a dropdown in the sidebar.

## 0.4.4 — 2026-11-21

- Updates come with release notes. Before you install, "What's new" shows the new version's changes. After an update, the first launch shows what changed.
- Project docs refresh: a Refresh button, plus an automatic refresh while the Docs tab is open. The open document reloads when its file changes.
- New agent skill, "suggest documents": proposes which project documents belong on which tasks. Nothing is linked until you approve it.
- The Recent tab has two views: Tasks, and History. Each history entry opens the task it changed.
- Agents work on the board for their project folder. They no longer write to the default board by accident.
- Agents that ask for a new board send a request. Nothing is created until you approve it on the board.
- New app icon.

## 0.4.3 — 2026-11-21

- One colour theme for light and dark, rebuilt from a single set of tokens. Dropdowns and the workspace menu are readable in dark mode.
- Agent runs page: skills on the left, one form for the chosen skill, and the request queue below. Typed text is kept while the page refreshes.
- Agents follow the board: they pick up tasks by priority and keep their status and progress up to date.

## 0.4.2 — 2026-11-21

- Fixed agent folder suggestions failing with "Failed to fetch".
- Fixed renaming a workspace: the name now updates in the picker and the breadcrumb.

## 0.4.1 — 2026-11-21

- Agents can suggest a project folder. You accept or dismiss the suggestion in the Docs tab. Nothing connects until you accept.

## 0.4.0 — 2026-11-21

- Docs tab: a read-only view of each workspace's project folder, with Markdown rendering.
- Tasks can link to project documents.
- New agent tools to list and read project documents.
- The sidebar stays the height of the window while the board scrolls.

## 0.3.1 — 2026-11-21

- Dark mode.
- Workspace picker with a friendlier palette and motion.

## 0.3.0 — 2026-11-21

- Separate workspaces, each with its own board.
- Change history for each task: who changed it (you, the API, or an agent), and which client and model.
- Clankban checks for updates in the background and tells you when one is ready.
- Fixed `update_task` dropping the checklist.
