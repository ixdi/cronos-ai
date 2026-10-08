# Cronos-AI: Agent-Oriented Software Factory

Cronos-AI is a multi-agent architecture designed to create complete products from scratch. It is based on the concept of "software factories" and uses a workflow supervised by humans as an execution environment to manage multiple agents working in parallel.

Within a task, workflow steps run sequentially: the next step starts only after the current step is complete. Independent sub-agents assigned to the same step run in parallel. In the first implementation, tasks themselves are processed sequentially; running multiple tasks in parallel is planned for a later approach.

If a sub-agent fails during a parallel step, cancel the other agents still running in that step, stop the workflow, and mark the task as Error. A human decides whether to retry or clarify the context.

The file `design/design.md` provides a detailed description of the agents used in Cronos-AI, their roles, and how they interact within the system. Each agent is designed to perform specific tasks, contributing to the overall goal of building software products efficiently.

Cronos-AI is developed with a focus on modularity, allowing it to handle new features easly for Cronos-AI, when required.

The development of Cronos-AI follows a structured approach, ensuring that each agent can be independently developed, tested, and integrated into the Cronos-AI system. This modularity not only enhances maintainability but also allows for scalability as new agents can be added to the architecture without disrupting existing functionality.

Use Openspec to generate Cronos-AI.

Cronos-AI will start as a cli application, providing a tui interface for users to interact with the system. When start, it will open herdr, the tui, and pi orchestrator. If some tools are not installed, it will prompt the user to install them.

The code must have unit testing and e2e testing for each spec or task. The tests will be run automatically when the code is built, ensuring that any changes made to the system do not introduce regressions or break existing functionality.

Code security verification and code quality checks will be performed automatically during the build process, ensuring that the code adheres to best practices and is free from vulnerabilities.

The creation of Cronos-AI must specify or create the skills, extensions and mcp definitions required for the agents to perform their tasks effectively. These definitions will serve as a blueprint for the agents, guiding their behavior and interactions within the system.

This first implementation will allow users to create a project, create its tasks and store them, initiate a task and its workflow agents, monitor agent activities, and manage the overall task development process following the defined steps. For each task, those steps run sequentially, while independent sub-agents within a step run in parallel.

A later approach will allow multiple tasks to run in parallel, enabling the system to handle complex projects with agents working simultaneously on different tasks. This does not change the sequential order of steps within each task.
