# shellcheck shell=sh
# Kept tested release builds, shared by scripts/package-macos.sh,
# scripts/macos-real-selftest.sh and scripts/windows-app.sh (sourced).
#
# A build that passed the tests is copied to a store that outlives the folder
# the build was made in (a worktree or a Windows worker, both deleted after a
# merge), under ROOT/<tree hash>/. A release packages the exact binaries the
# tests ran by finding them there. The store holds the newest KEEP trees
# (NVRVIEW_KEPT_BUILDS, default 3); saving prunes the older ones. Plain POSIX sh.

tb_keep_default="${NVRVIEW_KEPT_BUILDS:-3}"

# tb_save ROOT TREE ITEM...: copies the files and folders ITEM (each under its
# own name) to ROOT/TREE, replacing an earlier copy, then drops all but the
# newest trees. The copy is made beside the final folder and moved in, so a
# store never holds a half-written tree.
tb_save() {
  tb_root="$1"; tb_tree="$2"; shift 2
  mkdir -p "$tb_root"
  rm -rf "${tb_root:?}/$tb_tree.new"
  mkdir "$tb_root/$tb_tree.new"
  for tb_item in "$@"; do
    cp -Rp "$tb_item" "$tb_root/$tb_tree.new/" || { rm -rf "${tb_root:?}/$tb_tree.new"; return 1; }
  done
  rm -rf "${tb_root:?}/$tb_tree"
  mv "$tb_root/$tb_tree.new" "$tb_root/$tb_tree"
  tb_prune "$tb_root"
}

# tb_list ROOT: the stored trees, newest first (by last save or use).
tb_list() {
  [ -d "$1" ] || return 0
  # shellcheck disable=SC2010,SC2012 # directory names are tree hashes
  ls -1t "$1" 2>/dev/null | grep -E '^[0-9a-f]{40,64}$' || true
}

# tb_prune ROOT [KEEP]: removes all but the newest KEEP trees, and any
# unfinished copy.
tb_prune() {
  rm -rf "${1:?}"/*.new
  tb_list "$1" | tail -n +"$(( ${2:-$tb_keep_default} + 1 ))" | while read -r tb_old; do
    rm -rf "${1:?}/$tb_old"
  done
}

# tb_touch ROOT TREE: marks a stored tree as just used, so it is the last to go.
tb_touch() { touch "$1/$2"; }
