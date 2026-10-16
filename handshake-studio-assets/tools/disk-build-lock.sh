# Disk-wide build lock (CX-984-002: at most one build per physical disk at a time), sourced by tools/run-round.sh.
# The workspace steward keeps a byte-identical copy for the team's cargo wrapper; steward sync refuses drift.
# Protocol hsk.studio.disk-build-lock@2, lock root R (one per disk; run-round resolves it from
# .config/disk-build-lock.toml or STUDIO_DISK_BUILD_LOCK_DIR):
#   identity     a process is msys_pid + msys_start (field 22 of /proc/PID/stat, fixed for the process lifetime) +
#                command (CX-VAL-004). Alive = /proc/PID/stat readable with the recorded start time. Dead (provably)
#                = /proc/PID gone (the MSYS pid table is shared by every Git Bash/MSYS process on the host, the
#                wrapped native cargo included) or the start time differs (pid reused). Missing or unreadable
#                identity = alive. Windows PIDs are recorded for operators only and never decide liveness.
#   held         R/held/owner: schema, token, class (round|builder), msys_pid, msys_start, win_pid, owner,
#                command, impl, max_hold_s, queued_at, acquired_at, acquired_epoch; the holder appends
#                child_msys_pid/child_msys_start for the wrapped child. Taken by `mv -T R/stage.<token> R/held`
#                (atomic; fails while R/held exists), so a held lock never lacks its owner file.
#   queue        R/queue/<class><EPOCHREALTIME digits>-<token> tickets (class 0 = round, 1 = builder) with the
#                waiter's identity; glob order is the queue order. Rounds go first (FIFO among rounds); builders
#                are FIFO among themselves and start only when no live round ticket waits, or in a round's gap.
#                Dead tickets are removed by any waiter.
#   gap          a round releasing between units writes R/gap (dbl_yield): the first queued builder may take ONE
#                hold and consumes the gap; the round's next ticket then goes first again. A gap with no builder
#                waiting, or not consumed within 15 s, is withdrawn by the round.
#   release      the holder renames R/held to R/released.<token> and deletes it. On Windows any open handle on
#                R/held/owner (a waiter reading it) makes the directory rename fail, so the rename is retried every
#                0.1-0.3 s (jitter) for STUDIO_DISK_BUILD_LOCK_RELEASE_S (default 60) s until R/held is gone or
#                carries another token; a failure is logged RELEASE_FAILED (stderr + R/events.log) and the token is
#                kept in DBL_UNRELEASED (not exported): the same process's next dbl_acquire renames its own leftover
#                first (RELEASE_RECOVERED) instead of waiting on itself; a process that exits leaves a dead holder
#                for the stale-break rule.
#   stale break  only when the holder and its recorded child are provably dead: mv -T R/held R/stale.<dead
#                token>; the target name makes the break a compare-and-swap on that token (a second breaker of
#                the same owner finds the name taken). A moved lock with another token is put back; if that
#                fails it is logged EXCLUSION_RISK. stale/released/stage/over leftovers older than 60 min are
#                deleted by the next acquirer.
#   fence        an acquirer that finds another schema in R/held, R/stage.* or R/queue exits 75 ("protocol
#                mismatch"); @1 code cannot break an @2 lock (no `pid` key = alive for it).
#   max hold     the holder records max_hold_s (STUDIO_DISK_BUILD_LOCK_MAX_HOLD_S; builder default 900, round
#                5400). A waiter that sees a holder past it writes ONE OVER_MAX_HOLD line to R/events.log (and
#                stderr) per holder; nobody force-breaks a live holder (process stops are Operator-approved).
#   re-entrant   STUDIO_DISK_BUILD_LOCK_TOKEN is exported to the holder's children; a request whose token equals
#                R/held's token runs without taking the lock again.
#   waiting      poll every STUDIO_DISK_BUILD_LOCK_POLL_S (default 1) s; a stderr line on the first wait and every
#                60 s; give up after STUDIO_DISK_BUILD_LOCK_TIMEOUT_S (default 7200) s with status 75.
#   set -e       safe for callers running with set -euo pipefail (the round entrypoint does).
#   cost         bash builtins on the hot path (process start is slow under MSYS): an uncontended acquire+release
#                forks mkdir, mv, rm (ticket), mv, rm; a poll forks sleep.
# Functions: dbl_acquire ROOT CLASS OWNER COMMAND IMPL (sets DBL_TOKEN, DBL_NESTED, DBL_WAITED_S, DBL_BROKE),
#   dbl_note_child PID, dbl_release, dbl_recover (own unreleased leftover), dbl_yield (release + open a gap; rounds), dbl_get KEY FILE (sets DBL_V),
#   dbl_field KEY FILE (prints), dbl_ident PID (sets DBL_START), dbl_alive FILE [PREFIX], dbl_holder_gone FILE.
DBL_SCHEMA=hsk.studio.disk-build-lock@2
dbl_get() {   # one mapfile read, so the handle is open only for that read (an open reader blocks the release rename)
  DBL_V=""
  local l
  local -a dbl_lines=()
  [ -r "$2" ] || return 0
  { mapfile -t dbl_lines <"$2"; } 2>/dev/null || :
  for l in "${dbl_lines[@]}"; do
    case "$l" in "$1="*) DBL_V=${l#"$1="}; return 0 ;; esac
  done
  return 0
}
dbl_field() { dbl_get "$1" "$2"; printf '%s\n' "$DBL_V"; }
dbl_now() { printf -v DBL_NOW '%(%s)T' -1; }
dbl_utc() { TZ=UTC0 printf -v DBL_UTC '%(%Y-%m-%dT%H:%M:%SZ)T' -1; }
dbl_ident() {   # PID -> DBL_START (start time field of /proc/PID/stat); status 1 when the process is gone
  local l="" rest
  DBL_START=""
  { IFS= read -r l || [ -n "$l" ]; } 2>/dev/null <"/proc/$1/stat" || :
  [ -n "$l" ] || return 1
  rest=${l##*) }
  local -a f=($rest)
  DBL_START=${f[19]:-}
  return 0
}
dbl_winpid() { local w=""; { read -r w || [ -n "$w" ]; } 2>/dev/null <"/proc/$1/winpid" || :; DBL_WINPID=${w:--}; }
# dbl_alive FILE [PREFIX]: 0 = alive or unknown, 1 = provably dead (see header).
dbl_alive() {
  local f=$1 p=${2:-} pid start
  dbl_get "${p}msys_pid" "$f"; pid=$DBL_V
  dbl_get "${p}msys_start" "$f"; start=$DBL_V
  [ -n "$pid" ] && [ -n "$start" ] || return 0
  dbl_ident "$pid" || return 1
  [ -z "$DBL_START" ] && return 0
  [ "$DBL_START" = "$start" ] && return 0
  return 1
}
dbl_holder_gone() {   # FILE: holder dead and (no child recorded or child dead)
  dbl_alive "$1" && return 1
  dbl_get child_msys_pid "$1"
  [ -z "$DBL_V" ] && return 0
  dbl_alive "$1" child_ && return 1
  return 0
}
dbl_event() {   # LINE: append to R/events.log and stderr
  dbl_utc
  printf '%s %s\n' "$DBL_UTC" "$1" >>"$DBL_ROOT/events.log" 2>/dev/null || :
  echo "disk-build-lock: $1" >&2
}
dbl_fence() {   # FILE: 0 when the record is ours (or absent/unreadable), 1 on another schema (in DBL_V)
  [ -f "$1" ] || return 0
  dbl_get schema "$1"
  [ -z "$DBL_V" ] || [ "$DBL_V" = "$DBL_SCHEMA" ]
}
dbl_acquire() {
  DBL_ROOT=$1 DBL_TOKEN="" DBL_NESTED=0 DBL_WAITED_S=0 DBL_BROKE=""
  local class=$2 owner=$3 cmd=$4 impl=$5 pid=${BASHPID:-$$} token stage held ticket t cur t0 last=-60 gone="" text
  local poll=${STUDIO_DISK_BUILD_LOCK_POLL_S:-1} limit=${STUDIO_DISK_BUILD_LOCK_TIMEOUT_S:-7200} cls=1 maxhold
  local first_round first_builder eligible over="" gapage ae mh
  held="$DBL_ROOT/held"
  if [ -n "${STUDIO_DISK_BUILD_LOCK_TOKEN:-}" ]; then
    dbl_get token "$held/owner"
    if [ "$DBL_V" = "$STUDIO_DISK_BUILD_LOCK_TOKEN" ]; then DBL_NESTED=1 DBL_TOKEN=$DBL_V; return 0; fi
  fi
  case "$class" in
    round) cls=0 maxhold=${STUDIO_DISK_BUILD_LOCK_MAX_HOLD_S:-5400} ;;
    *) class=builder maxhold=${STUDIO_DISK_BUILD_LOCK_MAX_HOLD_S:-900} ;;
  esac
  [ -d "$DBL_ROOT/queue" ] || mkdir -p "$DBL_ROOT/queue" || return 75
  dbl_now; t0=$DBL_NOW; dbl_utc; dbl_winpid "$pid"
  dbl_ident "$pid" || DBL_START=""
  token="$t0-$pid-$RANDOM$RANDOM"
  stage="$DBL_ROOT/stage.$token"
  ticket="$DBL_ROOT/queue/$cls${EPOCHREALTIME/[.,]/}-$token"
  cmd=${cmd//[$'\r\n\t']/ }
  printf -v text 'schema=%s\ntoken=%s\nclass=%s\nmsys_pid=%s\nmsys_start=%s\nwin_pid=%s\nowner=%s\ncommand=%s\nimpl=%s\nmax_hold_s=%s\nqueued_at=%s' \
    "$DBL_SCHEMA" "$token" "$class" "$pid" "$DBL_START" "$DBL_WINPID" "$owner" "${cmd:0:300}" "$impl" "$maxhold" "$DBL_UTC"
  printf '%s\n' "$text" >"$ticket" || return 75
  while :; do
    # Own leftover from a failed release in this process: rename it away instead of waiting on ourselves.
    [ -z "${DBL_UNRELEASED:-}" ] || dbl_recover
    # Fence: never mix protocol versions.
    if ! dbl_fence "$held/owner"; then
      rm -f "$ticket"; dbl_event "protocol mismatch: held has $DBL_V (this is $DBL_SCHEMA); refusing $owner"; return 75
    fi
    for t in "$DBL_ROOT"/stage.*/owner; do
      [ "$t" = "$stage/owner" ] && continue
      if ! dbl_fence "$t"; then rm -f "$ticket"; dbl_event "protocol mismatch: ${t%/owner} has $DBL_V; refusing $owner"; return 75; fi
    done
    # Queue order: first live round ticket, first live builder ticket; dead tickets are removed.
    first_round="" first_builder=""
    for t in "$DBL_ROOT"/queue/*; do
      [ -f "$t" ] || continue
      if ! dbl_fence "$t"; then rm -f "$ticket"; dbl_event "protocol mismatch: queue ticket ${t##*/} has $DBL_V; refusing $owner"; return 75; fi
      if [ "$t" != "$ticket" ] && ! dbl_alive "$t"; then rm -f "$t" || :; continue; fi
      case "${t##*/}" in 0*) [ -n "$first_round" ] || first_round=$t ;; *) [ -n "$first_builder" ] || first_builder=$t ;; esac
    done
    eligible=0
    if [ $cls -eq 0 ]; then
      if [ "$first_round" = "$ticket" ]; then
        eligible=1
        if [ -f "$DBL_ROOT/gap" ]; then
          dbl_get opened_epoch "$DBL_ROOT/gap"; dbl_now; gapage=$((DBL_NOW - ${DBL_V:-0}))
          if [ -n "$first_builder" ] && [ $gapage -lt 15 ]; then eligible=0; else rm -f "$DBL_ROOT/gap" || :; fi
        fi
      fi
    elif [ "$first_builder" = "$ticket" ] && { [ -z "$first_round" ] || [ -f "$DBL_ROOT/gap" ]; }; then
      eligible=1
    fi
    if [ $eligible -eq 1 ] && [ ! -d "$held" ]; then
      [ -f "$stage/owner" ] || { mkdir -p "$stage" && printf '%s\n' "$text" >"$stage/owner"; } || { rm -f "$ticket"; return 75; }
      if mv -T "$stage" "$held" 2>/dev/null; then
        DBL_TOKEN=$token
        export STUDIO_DISK_BUILD_LOCK_TOKEN=$token
        dbl_utc; dbl_now
        printf 'acquired_at=%s\nacquired_epoch=%s\n' "$DBL_UTC" "$DBL_NOW" >>"$held/owner"
        rm -f "$ticket" || :
        if [ $cls -eq 1 ] && [ -f "$DBL_ROOT/gap" ]; then mv -T "$DBL_ROOT/gap" "$DBL_ROOT/released.gap-$token" 2>/dev/null || :; fi
        DBL_WAITED_S=$((DBL_NOW - t0))
        [ $last -ge 0 ] && echo "disk-build-lock: acquired after ${DBL_WAITED_S}s ($class, $DBL_ROOT)" >&2
        if compgen -G "$DBL_ROOT/stale.*" >/dev/null || compgen -G "$DBL_ROOT/released.*" >/dev/null \
          || compgen -G "$DBL_ROOT/stage.*" >/dev/null || compgen -G "$DBL_ROOT/over.*" >/dev/null; then
          find "$DBL_ROOT" -maxdepth 1 \( -name 'stale.*' -o -name 'released.*' -o -name 'stage.*' -o -name 'over.*' \) \
            -mmin +60 -exec rm -rf {} + 2>/dev/null || :
        fi
        return 0
      fi
    fi
    if [ -f "$held/owner" ]; then
      dbl_get token "$held/owner"; cur=$DBL_V
      if [ -n "$cur" ] && { [ "$cur" = "$gone" ] || ! dbl_alive "$held/owner"; }; then
        gone=$cur
        dbl_get child_msys_pid "$held/owner"
        if { [ -z "$DBL_V" ] || ! dbl_alive "$held/owner" child_; } \
          && mv -T "$held" "$DBL_ROOT/stale.$cur" 2>/dev/null; then
          dbl_get token "$DBL_ROOT/stale.$cur/owner"
          if [ "$DBL_V" = "$cur" ]; then
            DBL_BROKE=$cur
            dbl_event "broke stale lock $cur (holder msys pid $(dbl_field msys_pid "$DBL_ROOT/stale.$cur/owner") and child $(dbl_field child_msys_pid "$DBL_ROOT/stale.$cur/owner") dead; $(dbl_field command "$DBL_ROOT/stale.$cur/owner"))"
          elif ! mv -T "$DBL_ROOT/stale.$cur" "$held" 2>/dev/null; then
            dbl_event "EXCLUSION_RISK: moved live lock $DBL_V while breaking $cur and could not put it back"
          fi
          continue
        fi
      elif [ -n "$cur" ] && [ "$over" != "$cur" ]; then
        dbl_get acquired_epoch "$held/owner"; ae=$DBL_V
        dbl_get max_hold_s "$held/owner"; mh=$DBL_V
        dbl_now
        if [ -n "$ae" ] && [ -n "$mh" ] && [ $((DBL_NOW - ae)) -gt "$mh" ]; then
          over=$cur
          if mkdir "$DBL_ROOT/over.$cur" 2>/dev/null; then
            dbl_event "OVER_MAX_HOLD $cur held $((DBL_NOW - ae))s > ${mh}s by $(dbl_field owner "$held/owner") msys pid $(dbl_field msys_pid "$held/owner") start $(dbl_field msys_start "$held/owner") win pid $(dbl_field win_pid "$held/owner") ($(dbl_field command "$held/owner")); lead: Operator PROCESS_STOP_APPROVED only, never force-break"
          fi
        fi
      fi
    fi
    dbl_now
    if [ $((DBL_NOW - t0)) -ge "$limit" ]; then
      rm -rf "$stage" "$ticket"
      echo "disk-build-lock: gave up after $((DBL_NOW - t0))s waiting for $(dbl_field owner "$held/owner") ($(dbl_field command "$held/owner"))" >&2
      return 75
    fi
    if [ $((DBL_NOW - t0 - last)) -ge 60 ]; then
      last=$((DBL_NOW - t0))
      echo "disk-build-lock: waiting ${last}s ($class; queue head round=${first_round##*/} builder=${first_builder##*/}) for $(dbl_field owner "$held/owner") since $(dbl_field acquired_at "$held/owner") ($(dbl_field command "$held/owner"))" >&2
    fi
    sleep "$poll"
  done
}
dbl_note_child() {   # PID: record the wrapped child so a killed holder with a live child is not broken
  [ -n "${DBL_TOKEN:-}" ] && [ "${DBL_NESTED:-0}" -eq 0 ] || return 0
  dbl_ident "$1" || DBL_START=""
  dbl_winpid "$1"
  printf 'child_msys_pid=%s\nchild_msys_start=%s\nchild_win_pid=%s\n' "$1" "$DBL_START" "$DBL_WINPID" >>"$DBL_ROOT/held/owner"
}
dbl_release() {
  if [ -z "${DBL_TOKEN:-}" ]; then [ -z "${DBL_UNRELEASED:-}" ] || dbl_recover; return 0; fi
  [ "${DBL_NESTED:-0}" -eq 0 ] || return 0
  local t0 window=${STUDIO_DISK_BUILD_LOCK_RELEASE_S:-60}
  dbl_now; t0=$DBL_NOW
  while :; do
    dbl_get token "$DBL_ROOT/held/owner"
    [ "$DBL_V" = "$DBL_TOKEN" ] || break
    if mv -T "$DBL_ROOT/held" "$DBL_ROOT/released.$DBL_TOKEN" 2>/dev/null; then
      rm -rf "$DBL_ROOT/released.$DBL_TOKEN" || :
      break
    fi
    dbl_now
    [ $((DBL_NOW - t0)) -lt "$window" ] || break
    sleep "0.$((1 + RANDOM % 3))"
  done
  dbl_get token "$DBL_ROOT/held/owner"
  if [ "$DBL_V" = "$DBL_TOKEN" ]; then
    DBL_UNRELEASED=$DBL_TOKEN DBL_UNRELEASED_ROOT=$DBL_ROOT
    dbl_event "RELEASE_FAILED $DBL_TOKEN: R/held could not be renamed for ${window} s (open readers); this process renames it on its next acquire or release"
  fi
  DBL_TOKEN=""
  unset STUDIO_DISK_BUILD_LOCK_TOKEN
  return 0
}
dbl_recover() {   # rename this process's own unreleased lock (DBL_UNRELEASED) away; one attempt
  dbl_get token "$DBL_UNRELEASED_ROOT/held/owner"
  if [ "$DBL_V" != "$DBL_UNRELEASED" ]; then DBL_UNRELEASED=""; return 0; fi
  if mv -T "$DBL_UNRELEASED_ROOT/held" "$DBL_UNRELEASED_ROOT/released.$DBL_UNRELEASED" 2>/dev/null; then
    rm -rf "$DBL_UNRELEASED_ROOT/released.$DBL_UNRELEASED" || :
    local DBL_ROOT=$DBL_UNRELEASED_ROOT
    dbl_event "RELEASE_RECOVERED $DBL_UNRELEASED: own leftover renamed by its holder process"
    DBL_UNRELEASED=""
  fi
  return 0
}
dbl_yield() {   # round between units: open a gap for one queued builder, then release (gap first: no lost gap)
  [ -n "${DBL_TOKEN:-}" ] && [ "${DBL_NESTED:-0}" -eq 0 ] || return 0
  dbl_now
  printf 'schema=%s\nround_token=%s\nopened_epoch=%s\n' "$DBL_SCHEMA" "$DBL_TOKEN" "$DBL_NOW" >"$DBL_ROOT/gap" || :
  dbl_release
  return 0
}
# Exported so child bash scripts of a holder (and of the cargo wrapper) can re-enter without re-sourcing.
export -f dbl_get dbl_field dbl_now dbl_utc dbl_ident dbl_winpid dbl_alive dbl_holder_gone dbl_event dbl_fence \
  dbl_acquire dbl_note_child dbl_release dbl_recover dbl_yield
export DBL_SCHEMA
