# GitHub App setup for Cronos delivery

Cronos uses a GitHub App installation token to push a task branch and create a pull request. Configure one target repository and base branch. Do not use a personal access token, a user OAuth token, or a broadly installed App.

## 1. Create the App

In GitHub Developer Settings, create a GitHub App for Cronos and configure only the repository permissions below:

| Permission | Access | Purpose |
| --- | --- | --- |
| Contents | Read and write | Push the host-created task branch. |
| Pull requests | Read and write | Create the pull request after human approval. |
| Metadata | Read-only (implicit) | GitHub requires this for installed Apps. |

Do not grant Actions/workflows, administration, issues, checks, or organization permissions. Cronos does not subscribe to webhooks or monitor PR/CI events after creating the pull request, so no webhook URL or event subscription is needed. Keep the App private to the organization/account that owns the configured repository.

Generate a private key for the App and store it only in the deployment secret manager. Treat downloaded PEM files as credentials: restrict filesystem access and remove temporary copies after configuring deployment secrets.

## 2. Install on exactly one repository

Install the App on the account that owns the configured repository and choose **Only select repositories**. Select the single Cronos target repository. Do not select all repositories.

Record these identifiers from GitHub's App/installation pages or API:

- App ID
- Installation ID
- Repository database ID (numeric repository ID, not the `owner/name` string)
- Repository owner and repository name

The repository ID is used to scope every installation token. The owner/name pair must identify that same repository for the HTTPS push and pull-request API calls.

## 3. Configure deployment secrets and environment

Set these values in the deployment's secret/environment configuration, not in a committed file:

| Environment variable | Value |
| --- | --- |
| `CRONOS_GITHUB_APP_ID` | GitHub App ID. |
| `CRONOS_GITHUB_APP_PRIVATE_KEY` | App private key PEM. Multiline PEM and escaped `\\n` newlines are accepted. |
| `CRONOS_GITHUB_INSTALLATION_ID` | Numeric installation ID. |
| `CRONOS_GITHUB_REPOSITORY_ID` | Numeric database ID of the one permitted repository. |
| `CRONOS_GITHUB_OWNER` | Repository owner/account name. |
| `CRONOS_GITHUB_REPOSITORY` | Repository name. |
| `CRONOS_BASE_BRANCH` | Existing target base branch, for example `main`. |

The CLI also needs the repository/runtime settings documented in [cronos-panel](../cronos-panel/README.md): `CRONOS_REPOSITORY_PATH`, `CRONOS_RUNTIME_ROOT`, `CRONOS_RUNTIME_IMAGE`, and `CRONOS_PI_CHAT_MODEL`.

Never put secret values in `bun.lock`, SQLite, task descriptions, checked-in `.env` files, Git remotes, or command-line arguments. The GitHub App private key is not forwarded into agent containers; the panel rejects GitHub private-key/token variable names in `CRONOS_RUNTIME_ENV`. Installation tokens are short-lived, repository-scoped, held only in private in-memory state, and refreshed before expiry. Push authentication is supplied to the Git subprocess through per-process environment configuration; it is not written to repository config. Git tracing is disabled for that subprocess and credentials are redacted from returned errors.

## 4. Verify permissions and the disposable-repository flow

Before production use, install the App on a disposable repository with the same two write permissions and provide the environment settings above through a secure local/deployment secret manager. Verify that:

1. the installation token is limited to that repository and has only `contents: write` and `pull_requests: write`;
2. an approved task creates a `cronos/<task-id>-<hash>` branch and a pull request against the configured base branch;
3. the task is marked `delivered` with the PR number, URL, and branch in SQLite; and
4. no later PR or CI event changes the task state.

The automated suite uses a local bare Git remote and mocked GitHub REST calls. It does not contact GitHub or create a remote repository. Do not run a manual integration test against a production repository.