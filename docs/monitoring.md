# Monitoring Cronos-AI work

Start `cronos-ai` without a subcommand to open the dashboard. The dashboard checks for `pi`, `herdr`, `openspec`, and `git` first; if any are missing, it asks you to install them and press Enter to retry. It never installs tools automatically.

## Navigation

- **Projects:** use ↑/↓ to select a project and Enter to open its task list. Active and archived projects are shown so archived work remains inspectable.
- **Tasks:** use ↑/↓ to select a task. Each row shows status, priority, and current workflow step. Press Enter for details.
- **Task details:** the summary shows status, priority, description, and active step. The activity panel lists recent agent updates; use ↑/↓ to select an update.
- Press **Backspace** or **Esc** to return one level, **r** to refresh immediately, and **q** to exit.

The dashboard refreshes persisted project/task state once per second, so status, step, and activity updates written by the workflow host appear without restarting. Activity is kept per task with a 100-event limit; older task files without activity data remain readable and show an empty activity panel.
