import { Octokit } from "@octokit/rest";
import { chmod, copyFile, lstat, mkdir, readlink, readdir, realpath, rm, symlink } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import type { TaskWorkspace } from "cronos-runtime/contracts";
import type { GitHubAppTokenProvider } from "./github-app-token";

type CommandOutput = { stdout: string; stderr: string; exitCode: number };
type PullRequestClient = {
  rest: {
    pulls: {
      create(input: {
        owner: string;
        repo: string;
        title: string;
        head: string;
        base: string;
        body: string;
      }): Promise<{ data: { number: number; html_url: string } }>;
    };
  };
};

type PullRequestClientFactory = (token: string) => PullRequestClient;
type PushBranch = (repositoryPath: string, branch: string, token: string) => Promise<void>;

export type GitHubDeliveryOptions = {
  repositoryPath: string;
  runtimeRoot: string;
  owner: string;
  repository: string;
  baseBranch: string;
  tokenProvider: Pick<GitHubAppTokenProvider, "getInstallationToken" | "invalidateToken">;
  gitBinary?: string;
  octokitFactory?: PullRequestClientFactory;
  pushBranch?: PushBranch;
};

export type PullRequestInput = {
  taskId: string;
  description: string;
  workspace: TaskWorkspace;
  documentationSummary: string;
  verificationSummary: string;
};

export type PullRequestResult = {
  branch: string;
  commitSha: string;
  number: number;
  url: string;
  title: string;
};

type PreparedBranch = {
  path: string;
  branch: string;
  commitSha: string;
  changedFiles: string[];
};

function isWithin(root: string, path: string): boolean {
  const rel = relative(root, path);
  return rel === "" || (rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel));
}

function safeGitEnvironment(overrides: Record<string, string> = {}): Record<string, string> {
  const env: Record<string, string> = { ...process.env } as Record<string, string>;
  const inheritedGitSettings = new Set([
    "GIT_CURL_VERBOSE",
    "GIT_ASKPASS",
    "GIT_ASKPASS_REQUIRE",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_CONFIG_PARAMETERS",
    "GIT_SSL_NO_VERIFY",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "SSH_ASKPASS",
  ]);
  for (const key of Object.keys(env)) {
    if (/^GIT_CONFIG_(COUNT|KEY_\d+|VALUE_\d+)$/.test(key) || /^GIT_TRACE/.test(key) || inheritedGitSettings.has(key)) {
      delete env[key];
    }
  }
  Object.assign(env, {
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_TERMINAL_PROMPT: "0",
  }, overrides);
  return env;
}

async function runGitRaw(
  binary: string,
  args: string[],
  options: { cwd?: string; env?: Record<string, string>; redactions?: string[] } = {},
): Promise<CommandOutput> {
  const child = Bun.spawn({
    cmd: [binary, ...args],
    cwd: options.cwd,
    env: safeGitEnvironment(options.env),
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  const redact = (value: string) => (options.redactions ?? []).reduce(
    (current, secret) => secret ? current.replaceAll(secret, "[REDACTED]") : current,
    value,
  );
  return { stdout, stderr: redact(stderr.trim()), exitCode };
}

async function runGit(
  binary: string,
  args: string[],
  options: { cwd?: string; env?: Record<string, string>; redactions?: string[] } = {},
): Promise<string> {
  const result = await runGitRaw(binary, args, options);
  if (result.exitCode !== 0) {
    throw new Error(`Git operation failed (${result.exitCode}): ${result.stderr || "no diagnostic output"}`);
  }
  return result.stdout.trim();
}

async function copyEntry(sourcePath: string, destinationPath: string): Promise<void> {
  const info = await lstat(sourcePath);
  if (info.isSymbolicLink()) {
    await symlink(await readlink(sourcePath), destinationPath);
  } else if (info.isFile()) {
    await copyFile(sourcePath, destinationPath);
    await chmod(destinationPath, info.mode & 0o111 ? 0o755 : 0o644);
  } else {
    throw new Error("Git ignore entry is not a regular file or symlink");
  }
}

async function copyTree(source: string, destination: string): Promise<void> {
  for (const entry of (await readdir(source, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.name === ".git") continue;
    const sourcePath = join(source, entry.name);
    const destinationPath = join(destination, entry.name);
    const info = await lstat(sourcePath);
    if (entry.name === ".gitignore" && (info.isFile() || info.isSymbolicLink())) continue;
    if (info.isSymbolicLink()) {
      await symlink(await readlink(sourcePath), destinationPath);
    } else if (info.isDirectory()) {
      await mkdir(destinationPath);
      await copyTree(sourcePath, destinationPath);
    } else if (info.isFile()) {
      await copyFile(sourcePath, destinationPath);
      await chmod(destinationPath, info.mode & 0o111 ? 0o755 : 0o644);
    } else {
      throw new Error(`Task workspace contains an unsupported filesystem entry: ${entry.name}`);
    }
  }
}

async function collectIgnoreFiles(root: string, relativeRoot = ""): Promise<string[]> {
  const result: string[] = [];
  for (const entry of await readdir(root, { withFileTypes: true })) {
    if (entry.name === ".git") continue;
    const childRelative = relativeRoot ? join(relativeRoot, entry.name) : entry.name;
    const childPath = join(root, entry.name);
    const info = await lstat(childPath);
    if (entry.name === ".gitignore" && (info.isFile() || info.isSymbolicLink())) {
      result.push(childRelative);
    } else if (info.isDirectory() && !info.isSymbolicLink()) {
      result.push(...await collectIgnoreFiles(childPath, childRelative));
    }
  }
  return result;
}

async function clearWorkingTree(path: string, preserve: Set<string>, relativeRoot = ""): Promise<void> {
  if (!relativeRoot) {
    const gitPath = join(path, ".git");
    const gitInfo = await lstat(gitPath);
    if (!gitInfo.isDirectory() || gitInfo.isSymbolicLink()) throw new Error("Host staging repository metadata is invalid");
  }
  for (const entry of await readdir(path)) {
    if (!relativeRoot && entry === ".git") continue;
    const relativePath = relativeRoot ? join(relativeRoot, entry) : entry;
    if (preserve.has(relativePath)) continue;
    const childPath = join(path, entry);
    const info = await lstat(childPath);
    if (info.isDirectory() && !info.isSymbolicLink()) {
      await clearWorkingTree(childPath, preserve, relativePath);
      if ((await readdir(childPath)).length === 0) await rm(childPath, { recursive: true, force: true });
    } else {
      await rm(childPath, { recursive: true, force: true });
    }
  }
}

async function applyTaskIgnoreFiles(binary: string, stagingPath: string, workspaceRoot: string, baseFiles: string[]): Promise<void> {
  const taskFiles = await collectIgnoreFiles(workspaceRoot);
  const allPaths = [...new Set([...baseFiles, ...taskFiles])];
  for (const path of allPaths) await rm(join(stagingPath, path), { recursive: true, force: true });
  for (const path of taskFiles) {
    const destination = join(stagingPath, path);
    await mkdir(dirname(destination), { recursive: true });
    await copyEntry(join(workspaceRoot, path), destination);
  }
  if (allPaths.length > 0) {
    await runGit(binary, ["-C", stagingPath, "add", "--force", "--all", "--", ...allPaths]);
  }
}

function branchName(taskId: string): string {
  const readable = taskId.toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 36) || "task";
  const suffix = createHash("sha256").update(taskId).digest("hex").slice(0, 10);
  return `cronos/${readable}-${suffix}`;
}

function safeSubject(value: string): string {
  return value.replace(/[\u0000-\u001f\u007f-\u009f]/g, " ").replace(/\s+/g, " ").trim().slice(0, 120) || "Task delivery";
}

async function prepareHostOwnedBranch(
  options: GitHubDeliveryOptions,
  input: PullRequestInput,
): Promise<PreparedBranch> {
  await mkdir(options.runtimeRoot, { recursive: true });
  const runtimeRoot = await realpath(options.runtimeRoot);
  const workspaceInfo = await lstat(input.workspace.rootPath);
  if (!workspaceInfo.isDirectory() || workspaceInfo.isSymbolicLink()) {
    throw new Error("Task workspace is not a regular directory");
  }
  const workspaceRoot = await realpath(input.workspace.rootPath);
  if (workspaceRoot === runtimeRoot || !isWithin(runtimeRoot, workspaceRoot)) {
    throw new Error("Task workspace is outside the configured runtime directory");
  }

  const binary = options.gitBinary ?? "git";
  const repositoryPath = await realpath(options.repositoryPath);
  const repositoryRoot = await runGit(binary, ["-C", repositoryPath, "rev-parse", "--show-toplevel"]);
  await runGit(binary, ["check-ref-format", "--branch", options.baseBranch]);
  const branch = branchName(input.taskId);
  const stagingPath = join(runtimeRoot, `delivery-${crypto.randomUUID()}`);
  const templatePath = join(runtimeRoot, `git-template-${crypto.randomUUID()}`);
  await mkdir(templatePath);
  let stagingCreated = false;

  try {
    await mkdir(stagingPath);
    stagingCreated = true;
    await runGit(binary, [
      "clone", "--no-hardlinks", "--single-branch", "--branch", options.baseBranch,
      "--template", templatePath, repositoryRoot, stagingPath,
    ]);
    await runGit(binary, ["-C", stagingPath, "checkout", "-B", branch]);
    const trackedFiles = await runGitRaw(binary, ["-C", stagingPath, "ls-files", "-z"]);
    if (trackedFiles.exitCode !== 0) throw new Error(`Could not inspect base files: ${trackedFiles.stderr}`);
    const baseIgnoreFiles = trackedFiles.stdout.split("\0")
      .filter((path) => path === ".gitignore" || path.endsWith("/.gitignore"));
    await clearWorkingTree(stagingPath, new Set(baseIgnoreFiles));
    await copyTree(workspaceRoot, stagingPath);
    await runGit(binary, ["-C", stagingPath, "add", "--all"]);
    await applyTaskIgnoreFiles(binary, stagingPath, workspaceRoot, baseIgnoreFiles);
    const staged = await runGitRaw(binary, ["-C", stagingPath, "diff", "--cached", "--quiet"]);
    if (staged.exitCode === 0) throw new Error("Task workspace contains no changes to deliver");
    if (staged.exitCode !== 1) throw new Error(`Could not inspect staged task changes: ${staged.stderr}`);
    const changedFilesOutput = await runGitRaw(binary, ["-C", stagingPath, "diff", "--cached", "--name-only", "-z"]);
    if (changedFilesOutput.exitCode !== 0) throw new Error(`Could not list staged files: ${changedFilesOutput.stderr}`);
    const changedFiles = changedFilesOutput.stdout.split("\0").filter(Boolean);

    const subject = safeSubject(input.description);
    await runGit(binary, [
      "-C", stagingPath,
      "-c", `core.hooksPath=${templatePath}`,
      "-c", "commit.gpgSign=false",
      "-c", "user.name=Cronos AI",
      "-c", "user.email=cronos-ai@users.noreply.github.com",
      "commit", "-m", `Cronos: ${subject}`,
    ]);
    const commitSha = await runGit(binary, ["-C", stagingPath, "rev-parse", "HEAD"]);
    const remote = `https://github.com/${options.owner}/${options.repository}.git`;
    await runGit(binary, ["-C", stagingPath, "remote", "set-url", "origin", remote]);
    return { path: stagingPath, branch, commitSha, changedFiles };
  } catch (error) {
    if (stagingCreated) await rm(stagingPath, { recursive: true, force: true }).catch(() => {});
    throw error;
  } finally {
    await rm(templatePath, { recursive: true, force: true }).catch(() => {});
  }
}

async function pushGitBranch(
  options: GitHubDeliveryOptions,
  repositoryPath: string,
  branch: string,
  token: string,
): Promise<void> {
  const auth = Buffer.from(`x-access-token:${token}`).toString("base64");
  const urlScope = `https://github.com/${options.owner}/${options.repository}.git`;
  const env: Record<string, string> = {
    GIT_CONFIG_COUNT: "2",
    GIT_CONFIG_KEY_0: `http.${urlScope}.extraheader`,
    GIT_CONFIG_VALUE_0: `AUTHORIZATION: basic ${auth}`,
    GIT_CONFIG_KEY_1: "credential.helper",
    GIT_CONFIG_VALUE_1: "",
  };
  await runGit(options.gitBinary ?? "git", [
    "-C", repositoryPath, "push", "--set-upstream", "origin", branch,
  ], { env, redactions: [token, auth] });
}

function defaultOctokitFactory(token: string): PullRequestClient {
  return new Octokit({
    auth: token,
    userAgent: "cronos-ai",
    log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
  }) as unknown as PullRequestClient;
}

function pullRequestBody(input: PullRequestInput, branch: string, changedFiles: string[]): string {
  const changedFilesSection = changedFiles.length
    ? changedFiles.map((file) => {
      const safePath = file.replaceAll(String.fromCharCode(96), "\\`").replace(/[\r\n]/g, " ");
      return `- \`${safePath}\``;
    }).join("\n")
    : "No changed files were reported.";
  return [
    `Cronos task: ${input.taskId}`,
    "",
    "## Task",
    input.description.slice(0, 12_000),
    "",
    "## Documentation summary",
    input.documentationSummary.slice(0, 12_000),
    "",
    "## Verification",
    input.verificationSummary.slice(0, 8_000),
    "",
    `Head branch: ${branch}`,
    "",
    "## Changed files",
    changedFilesSection,
  ].join("\n").slice(0, 60_000);
}

export class GitHubPullRequestService {
  private readonly octokitFactory: PullRequestClientFactory;
  private readonly pushBranch: PushBranch;

  constructor(private readonly options: GitHubDeliveryOptions) {
    if (!/^[A-Za-z0-9][A-Za-z0-9-]*$/.test(options.owner) || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(options.repository) || options.repository.endsWith(".")) {
      throw new Error("GitHub owner and repository must be valid path segments");
    }
    if (!options.baseBranch.trim() || options.baseBranch.startsWith("-")) {
      throw new Error("A valid GitHub base branch is required");
    }
    this.octokitFactory = options.octokitFactory ?? defaultOctokitFactory;
    this.pushBranch = options.pushBranch ?? ((repositoryPath, branch, token) =>
      pushGitBranch(options, repositoryPath, branch, token));
  }

  async createPullRequest(input: PullRequestInput): Promise<PullRequestResult> {
    if (input.workspace.taskId !== input.taskId) throw new Error("Task workspace does not match delivery task");
    const prepared = await prepareHostOwnedBranch(this.options, input);
    try {
      let token = await this.options.tokenProvider.getInstallationToken();
      try {
        await this.pushBranch(prepared.path, prepared.branch, token);
      } catch (error) {
        if (!this.isAuthenticationFailure(error)) throw this.redactError(error, token);
        this.options.tokenProvider.invalidateToken();
        token = await this.options.tokenProvider.getInstallationToken();
        await this.pushBranch(prepared.path, prepared.branch, token).catch((retryError) => {
          throw this.redactError(retryError, token);
        });
      }

      const title = `Cronos: ${safeSubject(input.description)}`;
      const body = pullRequestBody(input, prepared.branch, prepared.changedFiles);
      for (let attempt = 0; attempt < 2; attempt += 1) {
        try {
          const { data } = await this.octokitFactory(token).rest.pulls.create({
            owner: this.options.owner,
            repo: this.options.repository,
            title,
            head: prepared.branch,
            base: this.options.baseBranch,
            body,
          });
          let pullRequestUrl: URL;
          try {
            pullRequestUrl = new URL(data.html_url);
          } catch {
            throw new Error("GitHub returned an invalid pull request reference");
          }
          const expectedPrefix = `/${this.options.owner}/${this.options.repository}/pull/`.toLowerCase();
          if (
            !Number.isSafeInteger(data.number) ||
            data.number <= 0 ||
            pullRequestUrl.origin !== "https://github.com" ||
            !pullRequestUrl.pathname.toLowerCase().startsWith(expectedPrefix)
          ) {
            throw new Error("GitHub returned an invalid pull request reference");
          }
          return { branch: prepared.branch, commitSha: prepared.commitSha, number: data.number, url: data.html_url, title };
        } catch (error) {
          if (attempt === 0 && this.isAuthenticationFailure(error)) {
            this.options.tokenProvider.invalidateToken();
            token = await this.options.tokenProvider.getInstallationToken();
            continue;
          }
          throw this.redactError(error, token);
        }
      }
      throw new Error("GitHub pull request creation failed after token refresh");
    } finally {
      const root = await realpath(this.options.runtimeRoot);
      if (resolve(prepared.path) !== prepared.path || !isWithin(root, prepared.path)) {
        throw new Error("Refusing to remove delivery staging directory outside runtime root");
      }
      await rm(prepared.path, { recursive: true, force: true });
    }
  }

  private isAuthenticationFailure(error: unknown): boolean {
    const status = typeof error === "object" && error !== null && "status" in error
      ? (error as { status?: unknown }).status
      : undefined;
    const message = error instanceof Error ? error.message : "";
    return status === 401 || /authentication failed|http basic: access denied|returned error: 401/i.test(message);
  }

  private redactError(error: unknown, token: string): Error {
    const message = error instanceof Error ? error.message : "Unknown delivery error";
    return new Error(message.replaceAll(token, "[REDACTED]"));
  }
}
