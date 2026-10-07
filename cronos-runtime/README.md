# Cronos Runtime

`cronos-runtime` provides the replaceable workspace and agent-execution adapters. The MVP Herdr provider creates a task-specific Git worktree, starts a Docker container with that worktree mounted at `/workspace`, creates a pane when an agent role needs it, and closes the pane/workspace after use. `PiCliAgentRunner` launches Pi JSON mode in that pane and reports lifecycle events and the final summary through the provider-neutral `AgentRunner` contract.

## Host prerequisites

- Bun (the repository's package manager and test runner), Git, Docker, and the Herdr CLI with its local server available.
- A configured Docker image for agent work. It must provide `sh`, Git, Bun for running project checks, and Pi CLI. Pi CLI 1.0.4 requires Node.js 22.19 or newer; install a compatible Node runtime in the image as well.
- A local Git repository and base branch that are accessible to the Cronos process.

The image is supplied as `runtimeImage`; Cronos does not build or publish one by default. Do not bake provider credentials into the image. The Herdr workspace provider runs containers on Docker's bridge network so Pi can reach its model provider; apply any stricter egress policy at the Docker/network layer.

## Pi model configuration

`PiCliAgentRunner` requires a chat model ID (for example, `provider/model-id`) for Pi's coding sessions. Configure a chat-capable model supported by the Pi CLI; Jev is **not** a chat model and cannot implement or review code.

The triage role separately enables Pi's `codemode` tool and instructs it to call TypeSafe's Jev classifier (`typesafe/jev-latest`). Jev returns structured classification answers. It is not selected with Pi's `--model` option. The configured chat model is still required for the Pi session that invokes codemode.

A runtime composition can be configured along these lines:

```ts
import { HerdrWorkspaceProvider } from "cronos-runtime/herdr-workspace-provider";
import { PiCliAgentRunner } from "cronos-runtime/pi-cli-agent-runner";

const chatModel = Bun.env.CRONOS_PI_CHAT_MODEL;
if (!chatModel) throw new Error("Set CRONOS_PI_CHAT_MODEL to a Pi chat model ID");

const workspaceProvider = new HerdrWorkspaceProvider({
  runtimeRoot: Bun.env.CRONOS_RUNTIME_ROOT ?? "./.cronos-runtime",
  runtimeImage: Bun.env.CRONOS_RUNTIME_IMAGE ?? "local/cronos-agent-runtime:latest",
  // Names only: Docker inherits these values from the Cronos process environment.
  runtimeEnv: ["OPENAI_API_KEY", "TYPESAFE_API_KEY"],
});
const agentRunner = new PiCliAgentRunner({ chatModel });
```

Replace `OPENAI_API_KEY` with the environment variable required by the selected chat provider. Pi also supports other provider environment variables and authentication methods; see the [Pi Providers guide](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/providers.md). `TYPESAFE_API_KEY` authenticates the Jev classifier.

Set these variables **before starting Cronos** so the Docker CLI receives them. `runtimeEnv` is an explicit allowlist of variable names: values are not copied into SQLite or added as literal command-line arguments. Pi and its shell tools can read the allowlisted values inside the task container, so forward only model-provider keys with the minimum practical scope. Never forward the GitHub App private key, broad host credentials, or unrelated secrets into an agent container. Keep real credentials out of source control, task prompts, logs, and Docker images.

For example, load `CRONOS_PI_CHAT_MODEL`, `CRONOS_RUNTIME_IMAGE`, and the selected provider credentials from the deployment's secret manager, then start the Cronos process. Do not put real values in this README or commit a populated `.env` file.

## Workspace and pane lifecycle

`HerdrWorkspaceProvider` takes a configured repository path and base branch when creating a task workspace. It creates a detached task worktree under `runtimeRoot`; the task's changes are isolated from the base worktree. The Docker container mounts the task worktree at `/workspace`, the repository's common Git metadata read-only, and only that task's per-worktree Git metadata read-write. This prevents the agent from changing shared refs or repository configuration. Agents can inspect changes and edit files, but staging/committing and PR operations belong to the host orchestrator. Temporary Pi JSONL/status files live in the task's per-worktree Git metadata; normal run artifacts are cleaned after completion, while crash leftovers remain outside the source tree until that task worktree is removed. Herdr panes are created only when a role runs and are closed when that run finishes.

The provider's `suspendWorkspace` operation closes the Herdr workspace and removes its owned Docker container while preserving the Git worktree and task-specific Git metadata. `restoreWorkspace` validates that persisted workspace identity/path still points to a worktree in the configured repository, then creates a fresh container and Herdr workspace around that same worktree. Cronos startup reconciliation suspends active runtimes and marks their tasks `interrupted`; it does not resume them automatically. Explicit resume requires both the LangGraph checkpoint and the matching SQLite workspace handle. Closing a workspace after successful delivery or terminal cleanup removes its container and task worktree.

`PiCliAgentRunner` runs Pi with `--mode json --no-session --model <configured-chat-model>`. It consumes JSONL events, reports start/tool/progress/completion/failure events, derives changed files from the task worktree, and interrupts the pane process on cancellation or timeout. Triage is restricted to read/codemode tools; implementation and documentation receive read, bash, edit, and write; code review and verification receive read and bash only. `herdrBinary` and `piBinary` can be overridden for nonstandard installations; the Herdr binary is on the Cronos host, while Pi must be installed in the runtime image.

The runtime container is a tool-execution boundary, not a network-egress boundary: model calls require outbound access, and code running in the agent container can use that network. Treat repository content and model output as untrusted.

## Development and tests

From the repository root:

```sh
bun install --frozen-lockfile
bun test
```

The ordinary suite uses fake CLI processes and does not make model calls. The Herdr/Docker integration test is opt-in; it creates and cleans up a temporary repository, worktree, container, and Herdr workspace. Its Pi executable is a fixture, so it needs no API credentials:

```sh
CRONOS_RUNTIME_INTEGRATION=1 bun test --timeout=120000 \
  cronos-runtime/src/herdr-workspace-provider.integration.test.ts
```

To additionally verify Docker environment-variable forwarding, supply the test-only fixture value (not a real secret):

```sh
CRONOS_RUNTIME_INTEGRATION=1 \
CRONOS_RUNTIME_TEST_SECRET=cronos-integration-fixture \
bun test --timeout=120000 cronos-runtime/src/herdr-workspace-provider.integration.test.ts
```

The integration test defaults to `python:3.12-alpine3.22` as a minimal shell-capable image; `CRONOS_RUNTIME_TEST_IMAGE` may select another image. This fixture image is only for the fake-CLI integration test, not for real agent runs. A real runtime image must satisfy the prerequisites above.
