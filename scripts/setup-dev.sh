#!/bin/sh
# One-time setup after cloning: install the sanitization hook and check identity.
set -e
cd "$(git rev-parse --show-toplevel)"

git config core.hooksPath .githooks
chmod +x .githooks/pre-commit scripts/check-sanitized.py

email=$(git config user.email || true)
case "$email" in
  *@users.noreply.github.com) echo "Commit email OK: $email" ;;
  *) echo "Set a GitHub noreply commit email for this repository:"
     echo '  git config user.email "<id>+<username>@users.noreply.github.com"' ;;
esac

denylist="$HOME/.config/nvrview/denylist.txt"
if [ ! -f "$denylist" ]; then
  echo "Optional: list your real addresses, UIDs and camera names (never passwords),"
  echo "one per line, in $denylist so the hook refuses them."
fi

command -v gitleaks >/dev/null 2>&1 || echo "Optional: install gitleaks for a local secret scan."
