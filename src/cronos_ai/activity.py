"""Sanitize untrusted worker text before retaining it as factory activity."""

from __future__ import annotations

import json
import os
import re

_MAX_ACTIVITY_SUMMARY_LENGTH = 512
_ENV_SECRET_NAME = re.compile(
    r"(?:KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|AUTH)",
    re.IGNORECASE,
)
_ANSI_ESCAPE = re.compile(r"\x1b(?:[@-_][0-?]*[ -/]*[@-~])")
_SECRET_PATTERNS = (
    re.compile(r"\bsk-[A-Za-z0-9_-]{16,}\b"),
    re.compile(r"\b(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,})\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    re.compile(r"\bAIza[0-9A-Za-z_-]{35}\b"),
    re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
    re.compile(r"(?i)\bBearer\s+[A-Za-z0-9._~+/-]+=*"),
)
_NAMED_SECRET = re.compile(
    r"(?i)\b((?:api[_-]?key|access[_-]?token|refresh[_-]?token|secret|password|credential))"
    r"(\s*[:=]\s*)(?:\"[^\"]*\"|'[^']*'|[^\s,;]+)"
)


def sanitize_activity_summary(summary: str) -> str | None:
    """Redact likely credentials and bound text, or suppress unsafe input."""
    if not isinstance(summary, str):
        return None

    sanitized = _ANSI_ESCAPE.sub("", summary)
    sanitized = "".join(
        character
        for character in sanitized
        if character.isprintable() or character in "\t\n\r"
    )
    sanitized = " ".join(sanitized.split())
    if not sanitized:
        return None

    if sanitized.startswith(("{", "[")):
        try:
            json.loads(sanitized)
        except json.JSONDecodeError:
            pass
        else:
            return None

    for pattern in _SECRET_PATTERNS:
        sanitized = pattern.sub("[REDACTED]", sanitized)
    sanitized = _NAMED_SECRET.sub(r"\1\2[REDACTED]", sanitized)

    environment_secrets = sorted(
        (
            value
            for name, value in os.environ.items()
            if _ENV_SECRET_NAME.search(name) and len(value) >= 8
        ),
        key=len,
        reverse=True,
    )
    for secret in environment_secrets:
        sanitized = sanitized.replace(secret, "[REDACTED]")

    if len(sanitized) > _MAX_ACTIVITY_SUMMARY_LENGTH:
        truncation_marker = "[truncated]"
        sanitized = (
            sanitized[: _MAX_ACTIVITY_SUMMARY_LENGTH - len(truncation_marker)].rstrip()
            + truncation_marker
        )
    return sanitized or None
