import {
  createAppAuth,
  type InstallationAccessTokenAuthentication,
  type InstallationAuthOptions,
} from "@octokit/auth-app";

const TOKEN_PERMISSIONS = Object.freeze({
  contents: "write",
  pull_requests: "write",
});
const DEFAULT_REFRESH_SKEW_MS = 60_000;

type InstallationAuth = (
  options: InstallationAuthOptions,
) => Promise<InstallationAccessTokenAuthentication>;

export type GitHubAppAuthFactory = (options: {
  appId: string;
  privateKey: string;
  installationId: number;
}) => InstallationAuth;

export type GitHubAppTokenConfig = {
  appId: string;
  installationId: number;
  repositoryId: number;
  privateKey: string;
  refreshSkewMs?: number;
  now?: () => number;
  authFactory?: GitHubAppAuthFactory;
};

export type GitHubAppTokenEnvironment = Record<string, string | undefined>;

type CachedToken = {
  value: string;
  expiresAt: number;
};

function positiveInteger(value: string | undefined, setting: string): number {
  if (!value || !/^[1-9]\d*$/.test(value)) throw new Error(`Invalid ${setting} configuration`);
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed)) throw new Error(`Invalid ${setting} configuration`);
  return parsed;
}

function normalizePrivateKey(value: string): string {
  return value.replaceAll("\\n", "\n").trim();
}

export function githubAppTokenConfigFromEnvironment(env: GitHubAppTokenEnvironment): GitHubAppTokenConfig {
  const appId = env.CRONOS_GITHUB_APP_ID?.trim();
  const privateKey = env.CRONOS_GITHUB_APP_PRIVATE_KEY;
  if (!appId) throw new Error("Missing required environment variable CRONOS_GITHUB_APP_ID");
  if (!privateKey?.trim()) throw new Error("Missing required environment variable CRONOS_GITHUB_APP_PRIVATE_KEY");

  const config: GitHubAppTokenConfig = {
    appId,
    installationId: positiveInteger(env.CRONOS_GITHUB_INSTALLATION_ID, "GitHub installation ID"),
    repositoryId: positiveInteger(env.CRONOS_GITHUB_REPOSITORY_ID, "GitHub repository ID"),
    privateKey: normalizePrivateKey(privateKey),
  };
  Object.defineProperty(config, "privateKey", {
    value: config.privateKey,
    enumerable: false,
    writable: false,
    configurable: false,
  });
  return config;
}

export class GitHubAppTokenProvider {
  #auth: InstallationAuth;
  #installationId: number;
  #repositoryId: number;
  #refreshSkewMs: number;
  #now: () => number;
  #cachedToken: CachedToken | undefined;
  #pendingRequest: Promise<string> | undefined;
  #forceRefresh = false;

  constructor(config: GitHubAppTokenConfig) {
    if (!config.appId.trim()) throw new Error("GitHub App ID is required");
    if (!Number.isSafeInteger(config.installationId) || config.installationId <= 0) {
      throw new Error("GitHub installation ID must be a positive safe integer");
    }
    if (!Number.isSafeInteger(config.repositoryId) || config.repositoryId <= 0) {
      throw new Error("GitHub repository ID must be a positive safe integer");
    }
    if (!config.privateKey.trim()) throw new Error("GitHub App private key is required");

    this.#installationId = config.installationId;
    this.#repositoryId = config.repositoryId;
    this.#refreshSkewMs = config.refreshSkewMs ?? DEFAULT_REFRESH_SKEW_MS;
    if (!Number.isFinite(this.#refreshSkewMs) || this.#refreshSkewMs <= 0) {
      throw new Error("Token refresh skew must be a positive number");
    }
    this.#now = config.now ?? Date.now;

    const authFactory = config.authFactory ?? ((options) => createAppAuth({
      ...options,
      log: { warn: () => {} },
    }) as unknown as InstallationAuth);
    let auth: InstallationAuth;
    try {
      auth = authFactory({
        appId: config.appId,
        privateKey: normalizePrivateKey(config.privateKey),
        installationId: config.installationId,
      });
    } catch {
      throw new Error("GitHub App authentication configuration is invalid");
    }
    this.#auth = (options) => auth(options);
  }

  async getInstallationToken(): Promise<string> {
    const now = this.#now();
    if (
      !this.#forceRefresh &&
      this.#cachedToken &&
      this.#cachedToken.expiresAt - now > this.#refreshSkewMs
    ) {
      return this.#cachedToken.value;
    }
    if (this.#pendingRequest) return this.#pendingRequest;

    const refresh = this.#forceRefresh || this.#cachedToken !== undefined;
    this.#forceRefresh = false;
    const request = this.requestToken(refresh);
    this.#pendingRequest = request;
    try {
      return await request;
    } finally {
      if (this.#pendingRequest === request) this.#pendingRequest = undefined;
    }
  }

  invalidateToken(): void {
    this.#cachedToken = undefined;
    this.#forceRefresh = true;
  }

  private async requestToken(refresh: boolean): Promise<string> {
    let authentication: InstallationAccessTokenAuthentication;
    try {
      authentication = await this.#auth({
        type: "installation",
        installationId: this.#installationId,
        repositoryIds: [this.#repositoryId],
        permissions: TOKEN_PERMISSIONS,
        refresh,
      });
    } catch {
      throw new Error("GitHub App installation token request failed");
    }

    const expiresAt = Date.parse(authentication.expiresAt);
    const requestedRepositoryIds = authentication.repositoryIds?.map(Number) ?? [];
    const permissions = authentication.permissions ?? {};
    const permissionNames = Object.keys(permissions);
    const exactPermissions =
      permissions.contents === "write" &&
      permissions.pull_requests === "write" &&
      (permissions.metadata === undefined || permissions.metadata === "read") &&
      permissionNames.every((name) => name === "contents" || name === "pull_requests" || name === "metadata");
    if (
      authentication.type !== "token" ||
      authentication.tokenType !== "installation" ||
      authentication.installationId !== this.#installationId ||
      !authentication.token ||
      !Number.isFinite(expiresAt) ||
      expiresAt <= this.#now() ||
      authentication.repositorySelection !== "selected" ||
      requestedRepositoryIds.length !== 1 ||
      requestedRepositoryIds[0] !== this.#repositoryId ||
      !exactPermissions
    ) {
      throw new Error("GitHub App returned an installation token with an unexpected scope or expiration");
    }

    this.#cachedToken = { value: authentication.token, expiresAt };
    return authentication.token;
  }
}
