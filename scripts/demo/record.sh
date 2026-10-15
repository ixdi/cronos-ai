#!/bin/sh
# Records docs/tui-demo.mp4 and docs/tui-demo.gif: the real Cronos-AI TUI and task runner driving a
# fake project, with stand-ins for Herdr, Pi, and OpenSpec. Needs VHS (https://github.com/charmbracelet/vhs).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
demo=/tmp/cronos-ai-demo

command -v vhs > /dev/null || { echo "record.sh needs VHS: brew install vhs" >&2; exit 1; }
if [ -e "$demo" ]; then
  [ -e "$demo/.cronos-ai-demo" ] || { echo "$demo exists and is not a demo directory" >&2; exit 1; }
  rm -rf "$demo"
fi
mkdir -p "$demo/bin"
touch "$demo/.cronos-ai-demo"

cargo build --release --quiet --manifest-path "$root/Cargo.toml"
ln -s "$root/target/release/cronos-ai" "$demo/bin/cronos-ai"
"$root/scripts/demo/seed.sh" "$root/target/release/cronos-ai" "$demo"

export PATH="$demo/bin:$root/scripts/demo/bin:$PATH"
export CRONOS_AI_DATA_DIR="$demo/data" HERDR_ENV=1 CRONOS_AI_DEMO_REPO="$demo/storefront"
export GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME="Demo" GIT_AUTHOR_EMAIL="demo@example.invalid"
export GIT_COMMITTER_NAME="Demo" GIT_COMMITTER_EMAIL="demo@example.invalid"

cd "$demo"
vhs "$root/scripts/demo/tui-demo.tape"
mv tui-demo.mp4 tui-demo.gif "$root/docs/"
