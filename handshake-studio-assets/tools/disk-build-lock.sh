# Disk-wide build lock (CX-984-002: at most one build per physical disk at a time), sourced by tools/run-round.sh.
# The workspace steward keeps a byte-identical copy for the team's cargo wrapper; steward sync refuses drift.
# Protocol hsk.studio.disk-build-lock@1, lock root R (one per disk; run-round resolves it from
# .config/disk-build-lock.toml or STUDIO_DISK_BUILD_LOCK_DIR):
#   R/held/owner      the held lock: key=value lines schema, token, pid (MSYS), winpid, exe, owner, queued_at,
#                     command, impl, acquired_at; the holder appends child_pid/child_winpid/child_exe for a wrapped
#                     child process.
#   acquire           write the owner file into the private dir R/stage.<token> (mkdir), then `mv -T` that dir to
#                     R/held: one atomic rename, so a held lock never lacks its owner file; the rename fails while
#                     R/held exists.
#   release           only the holder (token match): mv -T R/held R/released.<token>, then delete that dir.
#   stale break       only when the holder AND its recorded child are provably gone. A pid is alive when `kill -0`
#                     succeeds (MSYS pid table, shared by every Git Bash/MSYS process on the host, Windows-native
#                     children included); otherwise it is gone only if its Windows PID is absent from `tasklist`
#                     or now runs a different image than recorded (/proc/PID/exename at record time). No tasklist
#                     (non-Windows): kill -0 decides. Unreadable owner, failing tasklist, reused live pid: alive.
#                     Break = mv -T R/held R/stale.<dead token>: the target name makes the break a compare-and-swap
#                     on that token (a second breaker of the same owner finds the target taken and fails); stale,
#                     released and stage dirs older than 60 min are deleted by the next acquirer.
#   re-entrant        STUDIO_DISK_BUILD_LOCK_TOKEN is exported to the holder's children; a request whose token
#                     equals R/held's token runs without taking the lock again.
#   waiting           poll every STUDIO_DISK_BUILD_LOCK_POLL_S (default 1) s; a stderr line on the first wait and
#                     every 60 s; give up after STUDIO_DISK_BUILD_LOCK_TIMEOUT_S (default 7200) s with status 75.
#   set -e            safe for callers running with set -euo pipefail (the round entrypoint does).
#   cost              bash builtins only on the hot path (process start is slow under MSYS): an uncontended
#                     acquire+release forks mkdir, mv, mv, rm; a poll forks sleep; tasklist runs only for a holder
#                     that already failed kill -0.
# Functions: dbl_acquire ROOT OWNER COMMAND IMPL (sets DBL_TOKEN, DBL_NESTED, DBL_WAITED_S, DBL_BROKE),
#   dbl_note_child PID, dbl_release, dbl_get KEY FILE (sets DBL_V), dbl_field KEY FILE (prints),
#   dbl_alive FILE [PREFIX], dbl_holder_gone FILE.
dbl_get() {
  DBL_V=""
  local l
  [ -r "$2" ] || return 0
  while IFS= read -r l || [ -n "$l" ]; do
    case "$l" in "$1="*) DBL_V=${l#"$1="}; return 0 ;; esac
  done 2>/dev/null <"$2" || :
  return 0
}
dbl_field() { dbl_get "$1" "$2"; printf '%s\n' "$DBL_V"; }
dbl_now() { printf -v DBL_NOW '%(%s)T' -1; }
dbl_utc() { TZ=UTC0 printf -v DBL_UTC '%(%Y-%m-%dT%H:%M:%SZ)T' -1; }
dbl_proc() {   # PID -> DBL_WINPID, DBL_EXE ('-' when unknown); /proc reads only
  local w="" e=""
  { read -r w || [ -n "$w" ]; } 2>/dev/null <"/proc/$1/winpid" || :
  { read -r e || [ -n "$e" ]; } 2>/dev/null <"/proc/$1/exename" || :
  e=${e##*/}
  case "$e" in "") e=- ;; *.exe|*.EXE) ;; *) e="$e.exe" ;; esac
  DBL_WINPID=${w:--} DBL_EXE=$e
}
# dbl_alive FILE [PREFIX]: 0 = alive or unknown, 1 = provably gone (see header).
dbl_alive() {
  local f=$1 p=${2:-} pid winpid exe out
  dbl_get "${p}pid" "$f"; pid=$DBL_V
  [ -n "$pid" ] || return 0
  kill -0 "$pid" 2>/dev/null && return 0
  dbl_get "${p}winpid" "$f"; winpid=$DBL_V
  dbl_get "${p}exe" "$f"; exe=$DBL_V
  if [ -n "$winpid" ] && [ "$winpid" != - ] && command -v tasklist >/dev/null 2>&1; then
    out=$(tasklist //FI "PID eq $winpid" //FO CSV //NH 2>/dev/null) || return 0
    out=${out#$'\r'}; out=${out#$'\n'}; out=${out#$'\r\n'}
    [ -n "$out" ] || return 0
    case "$exe" in ""|-) case "$out" in '"'*) return 0 ;; esac ;; esac   # image unknown: any process counts
    case "$out" in "\"$exe\","*) return 0 ;; esac
  fi
  return 1
}
dbl_holder_gone() {   # FILE: holder gone and (no child recorded or child gone)
  dbl_alive "$1" && return 1
  dbl_get child_pid "$1"
  [ -z "$DBL_V" ] && return 0
  dbl_alive "$1" child_ && return 1
  return 0
}
dbl_acquire() {
  DBL_ROOT=$1 DBL_TOKEN="" DBL_NESTED=0 DBL_WAITED_S=0 DBL_BROKE=""
  local owner=$2 cmd=$3 impl=$4 pid=${BASHPID:-$$} token stage held cur t0 last=-60 gone="" text
  local poll=${STUDIO_DISK_BUILD_LOCK_POLL_S:-1} limit=${STUDIO_DISK_BUILD_LOCK_TIMEOUT_S:-7200}
  held="$DBL_ROOT/held"
  if [ -n "${STUDIO_DISK_BUILD_LOCK_TOKEN:-}" ]; then
    dbl_get token "$held/owner"
    if [ "$DBL_V" = "$STUDIO_DISK_BUILD_LOCK_TOKEN" ]; then DBL_NESTED=1 DBL_TOKEN=$DBL_V; return 0; fi
  fi
  [ -d "$DBL_ROOT" ] || mkdir -p "$DBL_ROOT" || return 75
  dbl_now; t0=$DBL_NOW; dbl_utc; dbl_proc "$pid"
  token="$t0-$pid-$RANDOM$RANDOM"
  stage="$DBL_ROOT/stage.$token"
  cmd=${cmd//[$'\r\n\t']/ }
  printf -v text 'schema=hsk.studio.disk-build-lock@1\ntoken=%s\npid=%s\nwinpid=%s\nexe=%s\nowner=%s\nqueued_at=%s\ncommand=%s\nimpl=%s' \
    "$token" "$pid" "$DBL_WINPID" "$DBL_EXE" "$owner" "$DBL_UTC" "${cmd:0:300}" "$impl"
  while :; do
    if [ ! -d "$held" ]; then
      [ -f "$stage/owner" ] || { mkdir -p "$stage" && printf '%s\n' "$text" >"$stage/owner"; } || return 75
      if mv -T "$stage" "$held" 2>/dev/null; then
        DBL_TOKEN=$token
        export STUDIO_DISK_BUILD_LOCK_TOKEN=$token
        dbl_utc; printf 'acquired_at=%s\n' "$DBL_UTC" >>"$held/owner"
        dbl_now; DBL_WAITED_S=$((DBL_NOW - t0))
        [ $last -ge 0 ] && echo "disk-build-lock: acquired after ${DBL_WAITED_S}s ($DBL_ROOT)" >&2
        if compgen -G "$DBL_ROOT/stale.*" >/dev/null || compgen -G "$DBL_ROOT/released.*" >/dev/null \
          || compgen -G "$DBL_ROOT/stage.*" >/dev/null; then
          find "$DBL_ROOT" -maxdepth 1 \( -name 'stale.*' -o -name 'released.*' -o -name 'stage.*' \) -mmin +60 \
            -exec rm -rf {} + 2>/dev/null || :
        fi
        return 0
      fi
    fi
    if [ -f "$held/owner" ]; then
      dbl_get token "$held/owner"; cur=$DBL_V
      # Holder liveness: kill -0 first; tasklist only once per dead-holder candidate (cached in $gone).
      if [ -n "$cur" ] && { [ "$cur" = "$gone" ] || ! dbl_alive "$held/owner"; }; then
        gone=$cur
        dbl_get child_pid "$held/owner"
        if { [ -z "$DBL_V" ] || ! dbl_alive "$held/owner" child_; } \
          && mv -T "$held" "$DBL_ROOT/stale.$cur" 2>/dev/null; then
          dbl_get token "$DBL_ROOT/stale.$cur/owner"
          if [ "$DBL_V" = "$cur" ]; then
            DBL_BROKE=$cur
            echo "disk-build-lock: broke stale lock $cur (holder $(dbl_field pid "$DBL_ROOT/stale.$cur/owner"), child $(dbl_field child_pid "$DBL_ROOT/stale.$cur/owner") gone; $(dbl_field command "$DBL_ROOT/stale.$cur/owner"))" >&2
          else
            mv -T "$DBL_ROOT/stale.$cur" "$held" 2>/dev/null || :   # moved a different lock: put it back
          fi
          continue
        fi
      fi
    fi
    dbl_now
    if [ $((DBL_NOW - t0)) -ge "$limit" ]; then
      rm -rf "$stage"
      echo "disk-build-lock: gave up after $((DBL_NOW - t0))s waiting for $(dbl_field owner "$held/owner") pid $(dbl_field pid "$held/owner") ($(dbl_field command "$held/owner"))" >&2
      return 75
    fi
    if [ $((DBL_NOW - t0 - last)) -ge 60 ]; then
      last=$((DBL_NOW - t0))
      echo "disk-build-lock: waiting ${last}s for $(dbl_field owner "$held/owner") pid $(dbl_field pid "$held/owner") since $(dbl_field acquired_at "$held/owner") ($(dbl_field command "$held/owner"))" >&2
    fi
    sleep "$poll"
  done
}
dbl_note_child() {   # PID: record the wrapped child so a killed holder with a live child is not broken
  [ -n "${DBL_TOKEN:-}" ] && [ "${DBL_NESTED:-0}" -eq 0 ] || return 0
  dbl_proc "$1"
  printf 'child_pid=%s\nchild_winpid=%s\nchild_exe=%s\n' "$1" "$DBL_WINPID" "$DBL_EXE" >>"$DBL_ROOT/held/owner"
}
dbl_release() {
  [ -n "${DBL_TOKEN:-}" ] && [ "${DBL_NESTED:-0}" -eq 0 ] || return 0
  dbl_get token "$DBL_ROOT/held/owner"
  if [ "$DBL_V" = "$DBL_TOKEN" ] && mv -T "$DBL_ROOT/held" "$DBL_ROOT/released.$DBL_TOKEN" 2>/dev/null; then
    rm -rf "$DBL_ROOT/released.$DBL_TOKEN"
  fi
  DBL_TOKEN=""
  unset STUDIO_DISK_BUILD_LOCK_TOKEN
  return 0
}
# Exported so child bash scripts of a holder (and of the cargo wrapper) can re-enter without re-sourcing.
export -f dbl_get dbl_field dbl_now dbl_utc dbl_proc dbl_alive dbl_holder_gone dbl_acquire dbl_note_child dbl_release
