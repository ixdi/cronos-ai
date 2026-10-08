# Local storage format

Cronos-AI stores each project under the storage root supplied to `ProjectStore`:

```text
<root>/projects/<project-uuid>/project.json
<root>/projects/<project-uuid>/tasks/<task-uuid>.json
```

`project.json` contains the project ID, name, optional description, active/archived status, task IDs, and creation/update timestamps. Each task file contains its task and project IDs, title, optional description, priority, dependency IDs, workflow status, current step, step timestamps, recent agent activity (bounded to the latest 100 events), optional worktree reference, and creation/update timestamps. Older task files without `agent_activity` load with an empty activity list. IDs are UUIDs; timestamps are seconds since the Unix epoch. Enum values are serialized in `snake_case`.

Project and task text is validated before saving and after loading. Individual JSON files are limited to 1 MiB. Writes use a temporary file in the destination directory and atomically replace the destination. Invalid, oversized, symlinked, or identity-mismatched records are rejected; a failed update does not overwrite the existing record.

Task source code is kept in its Git worktree, not in JSON. The stored worktree reference is metadata and must be validated against the task's allowed worktree root before any later destructive filesystem operation.
