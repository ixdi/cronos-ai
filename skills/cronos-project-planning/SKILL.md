---
name: cronos-project-planning
description: Turn a Cronos-AI project's requirements into a small, ordered set of tasks that each go through specification, implementation, review, and merge on their own. Use when asked to plan a project's work from its requirements.
---

# Project planning

You read a project and its requirements and propose the tasks that will build it.
A human reviews your plan before any task is created, and each task later gets its own OpenSpec change, worktree, and review.
You can only read; do not try to change files.

## Explore before planning

1. Read the README and the top-level layout to learn what the project is and how it is built.
2. Read `openspec/specs/` to learn the behavior that already exists, capability by capability.
   A requirement that is already met needs no task, but it still needs a task that verifies it if nothing proves it today.
3. Read the code behind the capabilities the requirements touch, enough to judge how much work each one is.
4. Note the open tasks listed in the prompt; reuse their exact titles when they already cover work you would plan.

## Shape the tasks

- One task is one reviewable change: a capability, an endpoint, a screen, or a cross-cutting quality such as performance budgets or input validation.
- Prefer several small tasks over one large one, but never split work that cannot be merged on its own.
- Write imperative titles ("Add CSV export", "Stream large exports"), at most a short sentence.
- In the description, say what to build, what is out of scope, and how a reviewer can verify it.
- Cover every requirement.
  A functional requirement usually gets its own task; a non-functional requirement may be covered by the tasks it constrains, listed in their `requirements`.
- Add a dependency only when a task truly cannot start before another one is merged; dependencies must not form a cycle.
- Use `high` priority for foundations other tasks depend on and for risky work, `medium` for most features, and `low` for polish.

## Answer

End your answer with exactly one fenced `json` block in the format the prompt gives, and nothing after it.
Use the requirement numbers from the prompt (`R1`, `R2`, ...) and short, unique keys for `depends_on`.
