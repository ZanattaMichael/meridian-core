#!/bin/sh
# Puts a directory's files into a GitHub repository of the owner's.
#
#   tools/release/send_tree.sh <owner/repo> <dir> <commit message> <branch>
#
# An empty repository gets the files as its first commit, on main. One with
# history gets them on <branch>, with a draft pull request, so a changed
# formula or manifest is read before it lands. A repository that already
# holds exactly these files is left alone. Needs `gh` logged in (GH_TOKEN) and
# `gh auth setup-git`. Prints one line saying what happened, and writes it to
# $GITHUB_OUTPUT as `result` when running in Actions.
set -eu

repo="$1"
dir="$(cd "$2" && pwd)"
message="$3"
branch="$4"

say() {
  echo "$1"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "result=$1" >> "$GITHUB_OUTPUT"
  fi
}

if ! gh repo view "$repo" --json name > /dev/null 2>&1; then
  echo "::error::repository $repo does not exist or the token cannot see it; create it (private is fine) and give the token access to it"
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if ! git clone --quiet "https://github.com/$repo" "$work/repo" 2> "$work/clone.err"; then
  cat "$work/clone.err" >&2
  exit 1
fi
cd "$work/repo"

if ! git rev-parse --verify --quiet HEAD > /dev/null; then
  # No HEAD after a clone: the repository is empty, or its default branch
  # points nowhere while another branch exists (a repository first pushed to
  # from here, before GitHub moved its default). Use main if it is there.
  if git ls-remote --exit-code --heads origin main > /dev/null; then
    git checkout --quiet main
  fi
fi
if ! git rev-parse --verify --quiet HEAD > /dev/null; then
  # Empty: this is the first commit.
  git checkout --quiet -b main
  cp -R "$dir/." .
  git add -A
  git commit --quiet -m "$message"
  git push --quiet -u origin main
  say "first commit pushed to main"
  exit 0
fi

default="$(git rev-parse --abbrev-ref HEAD)"
cp -R "$dir/." .
if git diff --quiet && [ -z "$(git ls-files --others --exclude-standard)" ]; then
  say "already holds $message; nothing to do"
  exit 0
fi
if git ls-remote --exit-code --heads origin "$branch" > /dev/null; then
  say "branch $branch exists already; nothing pushed"
  exit 0
fi
git checkout --quiet -b "$branch"
git add -A
git commit --quiet -m "$message"
git push --quiet -u origin "$branch"
url="$(gh pr create --repo "$repo" --draft --head "$branch" --base "$default" \
  --title "$message" --body "Written by the release pipeline of https://github.com/${GITHUB_REPOSITORY:-ZanattaMichael/meridian-core}.")"
say "draft pull request $url"
