"""Local-first orchestration for human-supervised software delivery."""

from importlib.metadata import version

__version__ = version("cronos-ai")


def main() -> None:
    """Run the Cronos AI command-line interface."""
    from cronos_ai.cli import main as cli_main

    cli_main()
