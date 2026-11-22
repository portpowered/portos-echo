#!/bin/sh
# Coordinator only. Run on the Echo during the authorized T5 window.
exec 3>&2 2>/dev/null
export LC_ALL=C
umask 077

base=/data/local/hassmic
live=$base/hassmic
stage=$live.new
lock=$live.override.lock

fail() {
  printf '%s\n' hassmic >&3
  exit 1
}

executable() {
  [ ! -L "$1" ] && [ -f "$1" ] && [ -s "$1" ] && [ -x "$1" ]
}

digest() {
  hash=$(sha256sum <"$1") || return 1
  hash=${hash%% *}
  [ "${#hash}" -eq 64 ] || return 1
  case "$hash" in *[!0-9a-f]*) return 1 ;; esac
  printf '%s\n' "$hash"
}

inode() {
  stat -Lc '%d:%i' "$1"
}

context() {
  label=$(ls -Zd "$1") || return 1
  label=${label%% *}
  case "$label" in u:object_r:*) printf '%s\n' "$label" ;; *) return 1 ;; esac
}

proc_argv() {
  # One read also handles a process that exits as its proc file is opened.
  dd if="$1/cmdline" bs=4096 count=1 | tr '\000' '\n'
}

instance() {
  newline='
'
  invocation=$(proc_argv "$1") || return 1
  case "$invocation" in "$live${newline}-P${newline}wyoming" | "$live${newline}-P${newline}wyoming$newline"*) ;; *) return 1 ;; esac
  case "$newline$invocation$newline" in *"$newline-S$newline"* | *"$newline-G$newline"* | *"$newline-T$newline"*) return 1 ;; esac
  # After the comm in /proc/PID/stat, ppid and starttime are fields 2 and 20.
  IFS= read -r record <"$1/stat" || return 1
  case "$record" in "$2 (hassmic) "*) ;; *) return 1 ;; esac
  IFS=' ' read -r state parent _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ start _ <<EOF
${record#*) }
EOF
  [ "$state" != Z ] && [ "$state" != X ] || return 1
  case "$start" in '' | *[!0-9]*) return 1 ;; esac
  case "$parent" in '' | *[!0-9]* | 0 | 1) return 1 ;; esac
  [ "$(stat -c '%u' "/proc/$parent")" = 0 ] || return 1
  command=$(proc_argv "/proc/$parent") || return 1
  # The pinned boot.sh execs sh with one of these scripts; runas execs its child.
  case "$command" in
    "sh
/system/hassmic/main.sh
satellite" | "sh
$base/ota/current/main.sh
satellite") ;;
    *) return 1 ;;
  esac
  printf '%s:%s\n' "$parent" "$start"
}

select_child() {
  child=
  for proc in /proc/[0-9]*; do
    IFS= read -r name <"$proc/comm" || continue
    [ "$name" = hassmic ] || continue
    [ "$(inode "$proc/exe")" = "$live_inode" ] || continue
    pid=${proc##*/}
    identified=$(instance "$proc" "$pid") || continue
    [ -z "$child" ] || fail
    child=$pid
    child_instance=$identified
  done
  [ -n "$child" ] && same_child
}

same_child() {
  [ "$(inode "/proc/$child/exe")" = "$live_inode" ] &&
    [ "$(instance "/proc/$child" "$child")" = "$child_instance" ]
}

latest_backup() {
  newest=
  # The UTC timestamp and fixed-width collision counter sort in C locale.
  for candidate in "$live".prev-*; do
    executable "$candidate" || continue
    printf '%s\n' "${candidate##*/}" | grep -Eq '^hassmic[.]prev-[0-9]{8}T[0-9]{6}Z-[0-9]{6}$' || continue
    newest=$candidate
  done
  [ -n "$newest" ] || return 1
  printf '%s\n' "$newest"
}

save_backup() {
  cp -p "$live" "$lock/previous" || fail
  executable "$lock/previous" && [ "$(digest "$lock/previous")" = "$before" ] || fail
  stamp=$(date -u '+%Y%m%dT%H%M%SZ') || fail
  sequence=0
  while [ "$sequence" -le 999999 ]; do
    backup=$(printf '%s.prev-%s-%06d' "$live" "$stamp" "$sequence")
    if [ ! -e "$backup" ] && [ ! -L "$backup" ]; then
      # Publish the completed backup in one step, with no overwrite.
      ln "$lock/previous" "$backup" || fail
      return
    fi
    sequence=$((sequence + 1))
  done
  fail
}

cleanup() {
  if [ "$staging" = yes ]; then rm -f "$stage"; fi
  rm -f "$lock/previous"
  rmdir "$lock"
}

case "$#:${1-}" in
  2:install)
    source=$2
    [ -n "$source" ] || fail
    case "$source" in /*) ;; *) source=$PWD/$source ;; esac
    ;;
  1:rollback) ;;
  *) fail ;;
esac

[ "$(id -u)" = 0 ] || fail
for folder in /data /data/local "$base"; do
  [ -d "$folder" ] && [ ! -L "$folder" ] || fail
done
[ "$(stat -c '%u:%a' "$base")" = 0:755 ] || fail
mkdir "$lock" || fail
staging=no
trap cleanup 0
trap 'exit 1' HUP INT TERM

executable "$live" && [ ! -e "$stage" ] && [ ! -L "$stage" ] || fail
live_inode=$(inode "$live") || fail
select_child || fail
kill -0 "$child" || fail
if [ "$1" = rollback ]; then source=$(latest_backup) || fail; fi
executable "$source" || fail
before=$(digest "$live") || fail
after=$(digest "$source") || fail
mode=$(stat -c '%u:%g:%a' "$live") || fail
label=$(context "$live") || fail

# Start with the launch metadata of the active override, then write the new bytes.
staging=yes
cp -p "$live" "$stage" || fail
cat <"$source" >"$stage" || fail
if [ "$(context "$stage")" != "$label" ]; then chcon "$label" "$stage" || fail; fi
executable "$stage" && [ "$(digest "$stage")" = "$after" ] &&
  [ "$(stat -c '%u:%g:%a' "$stage")" = "$mode" ] && [ "$(context "$stage")" = "$label" ] || fail
if [ "$1" = install ]; then save_backup; fi

executable "$live" || fail
[ "$(inode "$live")" = "$live_inode" ] || fail
same_child || fail
# Both paths are in the same directory. The live name stays present throughout.
mv -f "$stage" "$live" || fail
staging=no
printf 'hassmic %.12s %.12s\n' "$before" "$after"
# Compare /proc/exe with the original inode, which the child still has open.
same_child || fail
kill -TERM "$child" || fail
