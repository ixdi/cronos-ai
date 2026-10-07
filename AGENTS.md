# Cronos AI: Agent-Oriented Software Factory

Cronos AI is a multi-agent architecture designed to create complete products from scratch. It is based on the concept of
"software factories" and uses a workflow supervised by humans as an execution environment to manage multiple agents working in parallel.

Below is how to structure the workflow to create complete products from scratch using a multi-agent architecture.

## Cronos AI Architecture

Cronos AI is a repo, that is composed of 5 main components. The components communicate with each other via a Socket
API, and the orchestrator is the main component that controls the global state of the workflow.

The repo is managed with bun workspaces

When cronos-ai is started, it will start the orchestrator, which will then start the queue and the storage. The
orchestrator will instantiate a runtime when necessary.

- **cronos-core**: THE ORCHESTRATOR (Bun + Typescript + LangGraph.js). Controls the global state (via Socket API / CLI)
  - LOGGING (logtape) <── Logs and metrics for each task in the workflow
- **cronos-queue**: QUEUE (Bun + Typescript). Manages and caches the input tasks, waiting for the orchestrator to assign them to the triage agent in a new workflow
- **cronos-runtime**: THE RUNTIME (Herdr Runtime). Keeps sessions alive
  - It runs in a new git worktree. Each task is identified by a unique identifier and is executed in its own Herdr space.
  - Each runtime is run in a docker container, which isolates task execution and manages its dependencies. The
    container mounts common Git metadata read-only and only its task's per-worktree metadata read-write, so agents
    cannot alter shared refs; the orchestrator host stages, commits, and creates the PR.
  - The long-term architecture supports multiple runtimes in parallel, one per task; the initial MVP runs only one task at a time.
  - Each input task is a Herdr space that represents a task with a workflows
  - Each panel in a space (Pane 1) (Pane 2) (Pane ...) has a specialized agent from the workflow for the task
  - Specialized agents for each task in the workflow
  - After the human approves the task, the orchestrator agent will create a PR (Pull Request) in the repository and will close the Herdr space for that task, and will continue with the next task in the queue.
- **cronos-panel**: CONTROL PANEL (Opentui + Herdr new space). TUI interface for Human monitoring and managment
- **cronos-storage**: STORAGE/DATABASE (SQLite + Bun/Typescript). Stores the queue of tasks, the state of each task, and the status of each agent in the workflow.
  - It can be used to restore the state of the workflow in case of a crash or restart.

## Cronos AI Workflow

Read the ./design/cronos_ai_workflow.mmd file in Mermaid to understand the desired workflow.

Each node in the workflow can be a decision made by the orchestrator, except that nodes where the human approve is required.

The agents are specialized in different tasks as defined by the workflow and are based on skills and mcp servers.

The workflow must also take into account, besides the code and tests:

- Documentation
- Secret envs configuration
- Security
- Compliance
- Dependencies
- Monitoring, logs, and alerts
- Repositories and versions

## Step-by-Step

### Step 1: Ingestion and Planning

- Different types of inputs can be ingested into the queue, such as requirements, features, bugs, and errors.
- An API (bun) processes the input and then inserts it into the queue for the orchestrator to process.
- The orchestrator agent reads the queue and send the tast to the triage agent, which will decide how to handle it based
  on the input.
  - The long-term workflow can process inputs in parallel; the initial MVP processes one task at a time.

### Step 2: Orchestration

- The orchestrator agent uses the [Herdr CLI](https://herdr.dev/docs/cli-reference/) or its [Socket API](https://herdr.dev/docs/socket-api/) to automatically spin up the workspace. Terminal execution: The Typescript/Bun script runs commands like:

```
herdr session create cronos-ai
herdr pane split --right
herdr pane split --bottom
```

- This creates different spaces in the terminal, so that each task has its own independent execution channel.

### Step 3: Specialized Execution (The Builders)

The orchestrator manages each workflow for a task and runs the specialized agents to resolve the input. Each agent is
specialized in a specific task. The workflow is managed with LangGraph.js, which allows to define the workflow and the
agents that will be used for each task. The orchestrator agent can also handle errors and conflicts, and can request
human intervention when necessary.

## Human Supervision (The Control Panel)

- The control panel is a TUI interface that allows the human to see the status of each task and
  approve or reject them.
- The orchestrator agent can also send notifications to the human when intervention is required.
- The Control Panel shows the queue status and the status of each task (working, blocked, error, canceled, done,
  paused).
- The Control Panel monitors the workflow and the human approves or rejects tasks as needed. The
  orchestrator agent can also request human intervention when necessary.
- For final task review, a human rejection returns the task to the implementation agent; code review and verification run again before the next approval request.
- The human, using the Control Panel, can also provide feedback to the agents, which can be used to improve their performance in future tasks.
- The human can also use the control panel to view logs and metrics for each task, which can help identify
  bottlenecks and areas for improvement.
- The human can also use the control panel to view the overall progress state of the workflow and in which node it is running.
- The human can view the pending tasks of the queue
- The human can view the the tasks that have been completed, as well as the status of each task (working, blocked, error, canceled, done, paused). Also can view the PR (Pull Request) created for each task.
- The Control Panel TUI allows the human to add more skills or context to each agent, if needed.
- All the configuration and status of the Control Panel is stored in a SQLite that can be used to restore the state
  of the workflow in case of a crash or restart.

## Agents

### The Orchestrator Agent

- LangGraph.js coordinates specialized agent roles and lifecycles, launching the selected agent CLI in Herdr panes; Herdr supplies the workspace and panes.
- In the long-term target, multiple tasks run in parallel, each through its own specialized workflow; the initial MVP runs one task at a time. The orchestrator manages the queue and coordinates the agents in each workflow.
- The orchestrator agent can also handle errors and conflicts, and can request human intervention when necessary.
- It is an event driven architecture, where each agent can send events to the orchestrator agent, which can then decide
  how to handle them.
- The orchestrator agent also send events to the Control Panel to update the status of each task.
- The orchestrator agent can also send notifications to the Control Panel when human intervention is required.
- The orchestrator agent creates a PR (Pull Request) in the repository when the task is completed and approved by the
  human. The PR contains the code output, tests, documentation, and any other relevant information for the task.
- The orchestrator defines a data structure for the workflow, which is used to store the state of each task and the
  status of each agent. The data structure is stored in SQLite that can be used to restore the state of the workflow
  in case of a crash or restart.

### Triage Agent

- Orchestrator starts the triage agent in a new Herdr space, when orchestrator considers based on the limit of tasks to
  resolve in parallel.
- The triage agent is responsible for getting an input from the queue and adapting the different types, errors, features, and requirements into a format that can be used by the orchestrator and the other agents. It generates a PRD (Product Requirement Document).
- The triage agent also handles errors and conflicts, and can request human intervention when necessary. It handles resolving conflicts with the intent of the project.
- The triage agent can also send events to the orchestrator agent, which can then decide how to handle them.
- The triage agent is responsible for deciding which type of branch it should follow.
  - It can decide for human clarification and intervention. After that it will decide again.
  - If the input is automatically resolvable, it will send the input to the implementation agent, which will start
    resolving it.
  - If the input is complex, the triage agent will send it to the Specs agent, which will create a new spec for the input
    and send it to the human approval.

### Specs Agent

- The specs agent is responsible for creating new specs for the input adapted to the intent of the project.
- It can ask the human for clarification and intervention, and can also send events to the orchestrator agent, which can
  then decide how to handle them.
- After the spec is created it must be approved by the human, and then it will be sent to the implementation agent,
  which will start resolving it or resend to the spec agent to refine it.
- Specs are functional, non-functional, resources if necessary, and technical.

### Implementation Agent

- The implementation agent is responsible for resolving the task input and creating the code output.
- It considers the product intent, the architecture, the code design and the vertical slices

### Code Review Agent

- The code review agent is responsible for reviewing the code output created by the implementation agent.
- Runs static analysis, unit tests, integration tests, and e2e tests.

### Verification Agent

- The verification agent is responsible for verifying the code output created by the implementation agent.
- It runs functional tests, non-functional tests, and performance tests.
- Checks for security, compliance, and dependencies.
- Checks for code quality, maintainability, and scalability.
- After the verification is success it will require the human approval

### Documentation Agent

- After the code output is verified and approved by the human, the documentation agent is responsible for creating the
  documentation for the code output.
- It creates the documentation in a format that can be used by the human and the other agents.
- There is a single documentation agent that is responsible for creating the documentation for all the tasks in the
  workflow.
- The documentation output can be used by the human and the other agents.
- The documentatation agent also can produce images, videos, and diagrams to explain the task solution if necessary.

## Database and Storage

- A SQLite database is used to store the queue of tasks, and also all the states of the workflow, which can be used to restore the state of the workflow in case of a crash or restart.
- The database is also used to store the logs and metrics for each task in the workflow, which can be used to monitor
  the performance of the agents and the workflow.
- The database is also used to store the configuration of the workflow, which can be used to restore the state of the
  workflow in case of a crash or restart.
- The database is also used to store the status of each agent in the workflow, which can be used to monitor the
  performance of the agents and the workflow.
- The database is also used to store the status of each task in the workflow, which can be used to monitor the
  performance of the agents and the workflow.
- The database is also used to store the status of each task in the queue, which can be used to monitor the performance
  of the agents and the workflow.
- There is an option to use a different database, such as external PostgreSQL, if needed.

## Technology Stack

- The repo is managed with bun workspaces
- cronos-core
  - The orchestrator is built with Bun, Typescript, and LangGraph.js.
  - The workflow and agents are built with LangGraph.js.
  - The logging is built with logtape and stored in SQLite.
- cronos-runtime
  - Each cronos-runtime instance runs in a docker container, which allows to isolate the execution of each task and to manage the dependencies of each task.
  - The runtime is built with bun and Typescript.
  - The Triage Agent uses the Jev classifier model to route work to the automatic or future specification/clarification paths
  - The Specs are built and managed with OpenSpec and send to the Orchestrator to store them.
- cronos-panel
  - The control panel is built with Opentui typescript user interface (TUI) and Herdr new space.
- cronos-queue
  - The queue is built with SQLite and Bun/Typescript.
- cronos-storage
  - The database is built with SQLite and Bun/Typescript.
