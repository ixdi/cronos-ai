import { expect, test } from "bun:test";
import type { InstallationAccessTokenAuthentication } from "@octokit/auth-app";
import {
  GitHubAppTokenProvider,
  githubAppTokenConfigFromEnvironment,
} from "./github-app-token";
import type { GitHubAppAuthFactory } from "./github-app-token";

function tokenResponse(
  options: { installationId: number; repositoryId: number },
  token: string,
  expiresAt: number,
): InstallationAccessTokenAuthentication {
  return {
    type: "token",
    tokenType: "installation",
    installationId: options.installationId,
    token,
    createdAt: new Date(expiresAt - 3_600_000).toISOString(),
    expiresAt: new Date(expiresAt).toISOString(),
    repositorySelection: "selected",
    repositoryIds: [options.repositoryId],
    permissions: { contents: "write", pull_requests: "write", metadata: "read" },
  };
}

test("requests only the configured repository and minimal write permissions, then refreshes safely", async () => {
  let now = Date.parse("2026-01-01T00:00:00.000Z");
  let tokenNumber = 0;
  const requests: Array<{ type: string; installationId?: number | string; repositoryIds?: (number | bigint)[]; repositoryNames?: string[]; permissions?: Record<string, string>; refresh?: boolean }> = [];
  const factory: GitHubAppAuthFactory = (app) => {
    expect(app.appId).toBe("app-17");
    expect(app.privateKey).toBe("-----BEGIN PRIVATE KEY-----\nredacted-private-key\n-----END PRIVATE KEY-----");
    return async (options) => {
      requests.push(options);
      await Promise.resolve();
      tokenNumber += 1;
      return tokenResponse(
        { installationId: Number(options.installationId), repositoryId: 445566 },
        `installation-token-${tokenNumber}`,
        now + 3_600_000,
      );
    };
  };
  const provider = new GitHubAppTokenProvider({
    appId: "app-17",
    installationId: 123,
    repositoryId: 445566,
    privateKey: "-----BEGIN PRIVATE KEY-----\\nredacted-private-key\\n-----END PRIVATE KEY-----",
    now: () => now,
    authFactory: factory,
  });

  const [first, concurrent] = await Promise.all([
    provider.getInstallationToken(),
    provider.getInstallationToken(),
  ]);
  expect(first).toBe("installation-token-1");
  expect(concurrent).toBe(first);
  expect(requests).toHaveLength(1);
  expect(requests[0]).toMatchObject({
    type: "installation",
    installationId: 123,
    repositoryIds: [445566],
    permissions: { contents: "write", pull_requests: "write" },
    refresh: false,
  });
  expect(requests[0]?.repositoryNames).toBeUndefined();
  expect(Object.keys(requests[0]?.permissions ?? {}).sort()).toEqual(["contents", "pull_requests"]);

  now += 3_570_000;
  expect(await provider.getInstallationToken()).toBe("installation-token-2");
  expect(requests[1]?.refresh).toBe(true);
  provider.invalidateToken();
  expect(await provider.getInstallationToken()).toBe("installation-token-3");
  expect(requests[2]?.refresh).toBe(true);

  const serializedProvider = JSON.stringify(provider);
  expect(serializedProvider).not.toContain("redacted-private-key");
  expect(serializedProvider).not.toContain("installation-token-");
});

test("loads deployment credentials without logging them and rejects tokens with excess scope", async () => {
  const config = githubAppTokenConfigFromEnvironment({
    CRONOS_GITHUB_APP_ID: "17",
    CRONOS_GITHUB_INSTALLATION_ID: "123",
    CRONOS_GITHUB_REPOSITORY_ID: "445566",
    CRONOS_GITHUB_APP_PRIVATE_KEY: "-----BEGIN PRIVATE KEY-----\\nprivate-key\\n-----END PRIVATE KEY-----",
  });
  expect(config).toMatchObject({ appId: "17", installationId: 123, repositoryId: 445566 });
  expect(config.privateKey).toContain("\nprivate-key\n");
  expect(JSON.stringify(config)).not.toContain("private-key");
  expect(() => githubAppTokenConfigFromEnvironment({})).toThrow("CRONOS_GITHUB_APP_ID");
  expect(() => githubAppTokenConfigFromEnvironment({
    CRONOS_GITHUB_APP_ID: "17",
    CRONOS_GITHUB_INSTALLATION_ID: "0",
    CRONOS_GITHUB_REPOSITORY_ID: "445566",
    CRONOS_GITHUB_APP_PRIVATE_KEY: "secret",
  })).toThrow("GitHub installation ID");

  const provider = new GitHubAppTokenProvider({
    appId: "17",
    installationId: 123,
    repositoryId: 445566,
    privateKey: "private-key",
    now: () => Date.parse("2026-01-01T00:00:00.000Z"),
    authFactory: () => async () => ({
      ...tokenResponse({ installationId: 123, repositoryId: 445566 }, "do-not-expose-this-token", Date.parse("2026-01-01T01:00:00.000Z")),
      permissions: { contents: "write", pull_requests: "write", workflows: "write" },
    }),
  });
  let errorMessage = "";
  try {
    await provider.getInstallationToken();
  } catch (error) {
    errorMessage = error instanceof Error ? error.message : "";
  }
  expect(errorMessage).toContain("unexpected scope");
  expect(errorMessage).not.toContain("do-not-expose-this-token");
});
