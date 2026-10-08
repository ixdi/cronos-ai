# Cronos-AI - AI Software Factory

It is a pi-mono workflow that processes tasks in a sequential manner.

It is an opinionated framework to create projects and process its tasks in a sequential manner.

Each project consists of several tasks, and each task follows a workflow steps, that allows to process tasks efficiently and effectively, ensuring that each task is completed accurately.

Each project and each task have a status, which is updated as the workflow progresses. The status of each task is
tracked in the project JSON file, allowing for easy monitoring. The status are:

- Pending: The task is waiting to be processed.
- In Progress: The task is currently being processed.
- Completed: The task has been successfully completed.
- Rejected: The task has been rejected and will not be processed further.
- Error: An error occurred during the processing of the task, and it requires attention.

## Technologies

- pi mono agent as orchestrator to manage the workflow and coordinate the execution of tasks.
- JSON project file format to store the tasks and several json files with the tasks associated data, allowing for easy manipulation and retrieval of information.
- Each task follows a defined workflow steps.
- herdr to review what is happening in the workflow and to manage the sub-agents.
- jev to prioritize the tasks and manage the dependencies between them.
- openspec to generate the specifications for each task.
- sub-agents pi extension to create sub-agents for each task, allowing for parallel processing and improved task management. Following the defined steps, the sub-agents will execute their assigned tasks and report back the results.
  - sub-agents follows the workflow defined in the steps, including prompt generation, code generation, code review, verification, and human approval. The workflow is defined as a pi workflow and is executed by the sub-agents in a sequential manner, ensuring that each task is completed accurately and efficiently.
- Each step agent type can have its own specific model, allowing for flexibility and adaptability in the workflow.
  - Each agent also has its own blueprint context, with extensions, mcp servers and skills, allowing for better understanding and execution of the tasks.
- A project is attached to a github repository. The commits after the human approval follow the commitlint and are uniquely and well-defined, allowing for easy tracking and management of the codebase.
- changelog follows the conventional commits format, allowing for easy tracking of changes and updates to the
  codebase.
- A Dashboard tui is used to create projects and with one selected, visualize the workflow's progress and provide real-time updates on the status of each task.
  - Use ratatui to create the dashboard tui.

## Dashboard

A Dashboard TUI is used to manage projects, to create, select, update or delete projects.

For each project it can visualize the workflow's progress, like a kanban, and provide real-time updates on the status of each task. This ensures that the work remains organized and efficient, even when handling a large number of tasks and that the human knows what is happening in each moment.

The dashboard uses a simple, modern, dark and intuitive tui interface to display the tasks, their priorities, and their current status. It allows for easy tracking of the workflow's progress and provides visibility into the overall task management process.

The tui shows some analytics about the tasks, including the number of tasks in each status, the average time taken to complete tasks, and the overall progress of the workflow. This provides valuable insights into the workflow's performance and allows for data-driven decision-making.

It also allows to click in each task to see the details of the task, including its specifications, code, diffs, and any associated resources. This provides a comprehensive view of each task and allows for easy access to the information needed for effective task management.

It also allows to approve or reject the changes in the specified human approval steps, providing a streamlined
process for validating the generated code and ensuring that it meets the necessary quality standards.

It allows to insert some notes, descriptions, or comments about the task, which can be used to provide additional
context or information for the task. This ensures that all relevant information is captured and easily accessible for future reference if it is approved or it is used as a reference for the next loop when a step is rejected in a human approval step.

A history of the task is also maintained, allowing for easy tracking of changes and updates over time. This ensures that all relevant information is captured and easily accessible for future reference. The tui can show this history in a simple and intuitive way, allowing for easy navigation and access to the information needed for effective task management.

The tui allows to insert a new task definition, which can be used to add new tasks to the workflow. This ensures that the workflow remains flexible and adaptable to changing requirements and allows for easy expansion of the task list. This will be used as the input for the first step of the workflow, which is the tasks collection step.

The status of each task is also displayed in the Dashboard TUI, providing real-time updates on the progress of the workflow.

## Storage

A core JSON file is used for each project to store the tasks of a project and their associated data, allowing for easy manipulation and retrieval of information. The JSON file is structured in a way that allows for easy access to the tasks and their associated data. It also contains the project definition, including the product requirements, functionals and not-functionals. An id is created for each project in the core json, which is used to identify the project and its tasks. This ensures that each project is uniquely identifiable and allows for easy tracking and management of the projects.

An id is created for each task in the core json, which is used to identify the task and its associated data in the JSON file. This ensures that each task is uniquely identifiable and allows for easy tracking and management of the tasks.

In the core json there is also a field for the timestamp of the task and for each of its steps, allowing for easy
tracking of the workflow's progress and providing visibility into the overall task management process and analysis.

For each task, a new json file with its id name is created in the tasks folder. It includes their inputs, specifications, promps, associated resources and others. It don't save the code in the JSON file, but it saves the path to the git worktree where the code is stored. This allows for easy access to the code and ensures that it is organized and accessible for future reference.

## Project definition

Allows to create the json core file with the project definition, including the product requirements, functionals and not-functionals.

It uses a pi agent that uses openspec to explore the requirements and generate the specifications for the project. This ensures that the project is well-defined and aligned with the overall objectives of the workflow.

For each requirement, a new task is created in the project core json file, allowing for easy tracking and management of the tasks. The tasks are then processed through the defined workflow steps, ensuring that each task is completed accurately and efficiently.

A project can be archived, which means that it is no longer active and will not be processed in the workflow.

## Steps for each project

Each step needs its own model, context, and skills, which is used to process the tasks and execute the workflow. The steps are defined in a sequential manner, ensuring that each task is completed accurately and efficiently.

### 1. Tasks Collection

The first step involves gathering tasks from user input. In the future more sources will be added.

- Task cleaning is performed to remove any irrelevant or duplicate tasks.
- Task transformation is applied to standardize the format of the tasks, making them easier to analyze.
- Task analysis is conducted to extract meaningful insights and patterns from the collected tasks.
- The collected tasks are then stored in a structured JSON file format for further processing.

### 2. Task Prioritization

In this step, the collected tasks are evaluated based on their importance and urgency. A prioritization algorithm is applied to rank the tasks accordingly. This ensures that the most critical tasks are addressed first, optimizing the workflow's efficiency.

A priority field is attached to each task in the JSON file, indicating its rank in the overall task list. This
allows for easy identification of high-priority tasks and facilitates effective cache task management.

It must take into account the dependencies between tasks, ensuring that tasks are executed in the correct order and that two dependency tasks can not run in parallel if one depends on the other.

Jev model is used in this step.

### 3. Prompt Generation

Once the next tasks selected, a prompt is generated for the task, providing clear instructions and context for the agent responsible for executing them.

The prompt is passed to the sub-agents extension. It is used to create sub-agents for each task, allowing for parallel processing and improved task management.

### 3. Sub-agents workflow

For each task, a sub-agent is created to handle its execution. The sub-agents operate independently, allowing for
concurrent processing of multiple tasks. Each sub-agent is responsible for executing its assigned task and reporting back the results.

Herdr is used to manage the sub-agents and review their activities. The Dashboard html page is updated in real-time to reflect the progress of each sub-agent, providing visibility into the overall workflow.

A new git worktree is used in this step. It is identified by the task id and is used to store the code generated for each task. This allows for easy tracking and management of the codebase, ensuring that each task's code is organized and accessible.

#### 3.1 Sub-agent Specs

Specs are generated for the task, including detailed descriptions, requirements, and any necessary resources. This step ensures that each task is well-defined and ready for execution.

Openspec is used to generate the specifications.

#### 3.2 Human Approval

A human approval step is included to review and validate the generated specs before proceeding with task execution.
This ensures that the tasks are aligned with the overall objectives and meet the necessary quality standards.

#### 3.3 Code generation

Once the specs are approved, code is generated for the task. This may involve writing scripts, creating functions, or developing algorithms to accomplish the task's objectives.

#### 3.4 Code Review

After the code is generated, a review process is conducted to ensure many steps that run in parallel. Each step runs in a different sub-agent.

- Code quality checks are conducted to ensure that the code adheres to best practices and coding standards.
- Performance optimization is carried out to enhance the efficiency and speed of the code.
- Testing is performed to verify, including unit tests, and integration tests, to ensure that the code functions correctly

#### 3.5 Verification

After the code review, verification steps are performed to ensure that the code meets the specified
requirements and functions as intended. The following sub-agents are involved in this process and run in parallel:

- Security checks are performed to identify and mitigate any potential vulnerabilities in the code.
- E2E testing is conducted to validate the overall functionality and that the code meets the specified requirements.

#### 3.6 Human aproval

After the verification steps, a final human approval is required to validate the code before deployment. This step
ensures that the code meets the necessary quality standards and aligns with the overall objectives of the workflow.

If human do not approve the code, the workflow returns to the code generation step to address any issues or concerns raised during the review process.

The human can also request changes, notes, or others about the code, which will be addressed in the next iteration of the workflow. The inputs are introduced into the tui.

#### 3.7 Documentation

After the code is approved successfully, documentation is generated to provide clear instructions and information about the code.
First in this step the agent will archive the specs from openspec in the task, create the needed commits to reflect the changes in the git worktree. A new release number is attached to this task.

After that, there are several sub-agents that run in parallel to generate the documentation. including:

- User guides are created to provide step-by-step instructions for using the code and its features.
- Technical documentation is generated to provide detailed information about the code's functions, parameters, and usage.
- Technical specifications are created to provide in-depth information about the code's architecture, design, and
- Changelog is generated to document any changes or updates made to the code, ensuring that users are aware of the latest features and improvements.

#### 3.8 Merge and Push

If human approve is successfull, after documentation finishes, this step merge the worktree into the main and push to the github repository.

The task is marked as completed in the JSON file, and the workflow continues to the next task in the queue.
