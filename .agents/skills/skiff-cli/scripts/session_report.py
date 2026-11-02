#!/usr/bin/env python3
"""Print a read-only Skiff session table. Requires Python 3, Rich and the skiff CLI."""

import argparse
from collections import Counter, defaultdict
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys

from rich import box
from rich.console import Console
from rich.table import Table
from rich.text import Text


def clean(value):
    """Keep titles and names from emitting terminal control sequences."""
    value = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", str(value or ""))
    return " ".join(re.sub(r"[\x00-\x1f\x7f-\x9f]", " ", value).split())


def read_json(*args):
    result = subprocess.run(
        ["skiff", *args, "--json"], capture_output=True, text=True, timeout=15
    )
    if result.returncode:
        raise RuntimeError(clean(result.stderr or result.stdout) or "skiff command failed")
    data = json.loads(result.stdout)
    if not isinstance(data, list):
        raise RuntimeError("Skiff returned an unexpected JSON format")
    return data


def members(node):
    if node.get("type") == "pane":
        yield node.get("session")
    elif node.get("type") == "split":
        yield from members(node.get("a") or {})
        yield from members(node.get("b") or {})


def report(sessions, groups, width, details=False):
    console = Console(width=width, highlight=False)
    counts = Counter(session.get("state", "unknown") for session in sessions)
    unread = sum(bool(session.get("unread")) for session in sessions)
    membership = {}
    active_ids = {session["id"] for session in sessions}
    empty_groups = []
    for group in groups:
        label = f"{clean(group.get('name') or 'unnamed')} [{clean(group['id'])}]"
        ids = set(members(group.get("layout") or {})) & active_ids
        if not ids:
            empty_groups.append((label, group.get("cwd")))
        for session_id in ids:
            membership[session_id] = label

    projects = defaultdict(list)
    for session in sessions:
        projects[clean(session.get("cwd") or "(no working directory)")].append(session)

    console.print(Text(f"Skiff · {len(sessions)} sessions · {unread} unread", style="bold"))
    states = [f"{count} {clean(state)}" for state, count in sorted(counts.items())]
    if states:
        console.print(Text(" · ".join(states), style="dim"))
    project_names = Counter(Path(cwd).name or cwd for cwd in projects)
    for cwd, project_sessions in sorted(projects.items()):
        console.print()
        project = Path(cwd).name or cwd
        if project_names[project] > 1:
            project = cwd
        table = Table(title=Text(project), title_justify="left", title_style="bold cyan",
                      box=box.ROUNDED, expand=True, show_lines=True,
                      border_style="dim", header_style="bold")
        narrow = width < 65
        table.add_column("Session", ratio=1 if narrow else None,
                         width=None if narrow else 23)
        table.show_header = not narrow
        if not narrow:
            table.add_column("State", width=7)
            table.add_column("Unread", width=6, justify="center")
            table.add_column("Title", ratio=1)
        if details:
            console.print(Text(cwd, style="dim"))
        for session in sorted(project_sessions, key=lambda s: (
                clean(s.get("name") or s.get("label")), s["id"])):
            program = clean(session.get("foreground_program") or session.get("command") or "unknown")
            name = clean(session.get("name") or program)
            state = clean(session.get("state") or "unknown")
            # A terminal title is context, not proof of a current task or completion.
            title = clean(session.get("title"))
            if re.match(r"\S+@\S+:", title):
                title = ""
            title = title.lstrip("✳✻✽✢✶✼✷✺✹✸✵✴✲✱* ")
            pane = Text(name, style="bold")
            if details:
                pane.append(f"\n{session['id']}\n{program}", style="dim")
                pane.append(f"\n{membership.get(session['id'], 'ungrouped')}", style="dim")
            style = {"working": "green", "waiting": "yellow", "done": "dim"}.get(state, "")
            # What the program reports itself (OSC 7501): what it waits for, a failed turn.
            reported = session.get("program_status") or {}
            if reported.get("state") == "blocked" and reported.get("kind"):
                state = f"{state} ({clean(reported['kind'])})"
            elif reported.get("state") == "error":
                state, style = f"{state} (failed)", "red"
            status = Text(state, style=style)
            if narrow:
                pane.append(f"\n{state}", style=style)
                if session.get("unread"):
                    pane.append(" · unread", style="cyan")
                if title:
                    pane.append(f"\n{title}", style="default")
                table.add_row(pane)
            else:
                table.add_row(pane, status,
                              Text("yes", style="cyan") if session.get("unread") else Text("—", style="dim"),
                              Text(title or "—", style="default" if title else "dim"))
        console.print(table)

    if not sessions:
        console.print("\nNo open sessions.")
    if details and empty_groups:
        console.print("\nEmpty groups", style="bold")
        for label, cwd in sorted(empty_groups):
            console.print(Text("  " + label + (f" | {clean(cwd)}" if cwd else "")))
    if details:
        console.print("\nIdle means quiet; it does not confirm task completion.", style="dim")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--width", type=int, default=shutil.get_terminal_size((38, 24)).columns,
                        help="output width, at least 24 columns (default: terminal width, or 38 when unavailable)")
    parser.add_argument("--mobile", action="store_true",
                        help="use a single-column table, at most 38 characters wide")
    parser.add_argument("--details", action="store_true",
                        help="include full paths, IDs, programs and groups")
    args = parser.parse_args()
    if args.width < 24:
        parser.error("--width must be at least 24")
    width = min(args.width, 38) if args.mobile else args.width
    try:
        sessions = read_json("ls")
        groups = read_json("group", "ls")
        report(sessions, groups, width, args.details)
    except FileNotFoundError:
        print("skiff is missing. In Skiff, use Help > Install skiff command.", file=sys.stderr)
        return 1
    except (RuntimeError, subprocess.TimeoutExpired, ValueError, KeyError, TypeError) as error:
        print(f"Cannot read Skiff sessions: {clean(error)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
