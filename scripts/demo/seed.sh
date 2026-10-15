#!/bin/sh
# Creates the demo's fake repositories and Cronos-AI data with the real CLI.
# Usage: seed.sh <cronos-ai binary> <demo directory>
set -eu
cronos=$1
demo=$2
data="$demo/data"
export CRONOS_AI_DATA_DIR="$data"
export GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME="Demo" GIT_AUTHOR_EMAIL="demo@example.invalid"
export GIT_COMMITTER_NAME="Demo" GIT_COMMITTER_EMAIL="demo@example.invalid"

repository() {
  repo="$demo/$1"
  mkdir -p "$repo/openspec/specs" "$repo/src" "$repo/scripts"
  git -C "$repo" init -q -b main
  printf '# %s\n' "$2" > "$repo/README.md"
  printf 'schema: spec-driven\n' > "$repo/openspec/config.yaml"
  printf '#!/bin/sh\nsleep 2\necho "42 tests passed"\n' > "$repo/scripts/check.sh"
  printf 'export {};\n' > "$repo/src/index.ts"
  git -C "$repo" add -A
  git -C "$repo" commit -q -m "chore: start $1"
}

id() { sed -n 's/^  "id": "\(.*\)",$/\1/p' | head -n 1; }

repository storefront "Storefront"
repository website "Company website"
repository mobile "Mobile app"

website=$("$cronos" project create "Company website" --description "Marketing pages and blog" --repository "$demo/website" | id)
mobile=$("$cronos" project create "Mobile app" --description "iOS and Android shopping app" --repository "$demo/mobile" | id)
shop=$("$cronos" project create "Storefront" --description "Online shop for handmade ceramics" --repository "$demo/storefront" | id)
for project in "$website" "$mobile" "$shop"; do
  "$cronos" project set-check-command "$project" -- sh scripts/check.sh > /dev/null
done
"$cronos" project select "$shop" > /dev/null

"$cronos" requirement add "$shop" "Shoppers can browse the catalog by category and search products by name." > /dev/null
"$cronos" requirement add "$shop" "Shoppers can add products to a cart and pay by card." > /dev/null
"$cronos" requirement add "$shop" "Every page loads in under 2 seconds on a 4G connection." --kind non-functional > /dev/null

task() { "$cronos" task create "$shop" "$@" | id; }
done1=$(task "Add product catalog API" --priority high)
done2=$(task "Build category pages" --priority medium --depends-on "$done1")
done3=$(task "Add product search" --priority medium --depends-on "$done1")
cart=$(task "Add shopping cart" --priority high --description "Keep a cart per shopper and show it at /cart." --depends-on "$done1")
task "Integrate card payments" --priority high --depends-on "$cart" > /dev/null
task "Write privacy policy page" --priority low > /dev/null
"$cronos" task create "$website" "Redesign the blog index" --priority medium > /dev/null
"$cronos" task create "$mobile" "Set up push notifications" --priority high > /dev/null

# Mark the first three tasks as completed a few days ago.
python3 - "$data/projects/$shop/tasks" "$done1" "$done2" "$done3" <<'PY'
import json, sys, time
folder, ids = sys.argv[1], sys.argv[2:]
now = int(time.time())
for days, task_id in zip((6, 4, 2), ids):
    path = f"{folder}/{task_id}.json"
    task = json.load(open(path))
    task["status"] = "completed"
    task["current_step"] = "local-merge"
    task["created_at_unix_secs"] = now - (days + 1) * 86400
    task["updated_at_unix_secs"] = now - days * 86400
    json.dump(task, open(path, "w"), indent=2)
PY
