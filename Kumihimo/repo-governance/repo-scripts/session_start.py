"""Read the portable session, canonical packet state and live Git state; never mutate."""
import json
import subprocess
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]


def read(relative):
    return yaml.safe_load((ROOT / relative).read_text(encoding="utf-8"))


def git(*arguments):
    return subprocess.check_output(["git", "-C", str(ROOT), *arguments], text=True).rstrip("\r\n")


def main():
    authority = read("repo-governance/codex/current/codex-source.yaml")
    session = read("repo-governance/sessions/current.yaml")
    packets = []
    for path in sorted((ROOT / "repo-governance/work-packets/active").glob("*/WP-*-state.yaml")):
        state = yaml.safe_load(path.read_text(encoding="utf-8"))
        if state["phase"] != "done":
            packets.append({key: state[key] for key in ("wp_id", "phase", "blockers", "next_action")})
    print(json.dumps({
        "session_id": session["session_id"],
        "dev_lead": session["dev_lead"],
        "workflow": authority.get("session_start", {}),
        "git": {"root": git("rev-parse", "--show-toplevel"), "branch": git("branch", "--show-current"),
                "revision": git("rev-parse", "HEAD"), "changes": git("status", "--short").splitlines()},
        "roles": session["roles"], "build_lane": session["build_lane"], "work_packets": packets,
        "next_action": session["next_action"],
    }, indent=2))


if __name__ == "__main__":
    main()
