from html import escape
from pathlib import Path
import re
import shutil
import subprocess


folder = Path(__file__).parent
index_path = folder / "index.html"
content_path = folder / "guide-content.html"
template_path = folder / "reference-page-template.html"
start_marker = "<!-- GUIDE_CONTENT_START -->"
end_marker = "<!-- GUIDE_CONTENT_END -->"
reference_pages = {
    "agent-profiles": "Set up step-specific Pi skills, extensions, and MCP servers for workflow agents.",
    "integration-contracts": "Understand how Cronos-AI coordinates Pi, Herdr, Git, and local state.",
    "monitoring": "Navigate the dashboard, review approvals, and recover or abandon tasks.",
    "project-task-commands": "Create and manage projects and tasks from the CLI or TUI.",
    "storage-format": "Learn where project and task records live and how Cronos-AI protects them.",
    "workflow-states": "Understand task queue behavior, workflow steps, approvals, and recovery.",
}

pandoc = shutil.which("pandoc")
if pandoc is None:
    raise SystemExit("Pandoc is required to build the HTML reference pages (install pandoc and rerun this script)")

page = index_path.read_text()
content = content_path.read_text().strip()
if page.count(start_marker) != 1 or page.count(end_marker) != 1:
    raise SystemExit("Expected exactly one guide content marker pair in index.html")
if start_marker in content or end_marker in content:
    raise SystemExit("Guide content must not contain build markers")
before, remainder = page.split(start_marker, 1)
_, after = remainder.split(end_marker, 1)
updated_index = f"{before}{start_marker}\n{content}\n{end_marker}{after}"

template = template_path.read_text()
rendered_pages = {}
for slug, summary in reference_pages.items():
    source = folder / f"{slug}.md"
    markdown_source = source.read_text()
    match = re.match(r"^#\s+(.+?)\s*(?:\r?\n|$)", markdown_source)
    if match is None:
        raise SystemExit(f"Expected a top-level title in {source}")
    title = match.group(1)
    rendered = subprocess.run(
        [pandoc, "--from=gfm", "--to=html5", "--wrap=none"],
        input=markdown_source[match.end():],
        capture_output=True,
        check=True,
        text=True,
    ).stdout.strip()
    rendered_pages[slug] = (
        template.replace("{{TITLE}}", escape(title))
        .replace("{{DESCRIPTION}}", escape(summary, quote=True))
        .replace("{{SUMMARY}}", escape(summary))
        .replace("{{CONTENT}}", rendered)
    )

if updated_index != page:
    index_path.write_text(updated_index)
print("Updated docs/index.html from docs/guide-content.html")
for slug, output in rendered_pages.items():
    (folder / f"{slug}.html").write_text(output)
    print(f"Updated docs/{slug}.html from docs/{slug}.md")
