#!/usr/bin/env python3
"""Conservative M0 import/I/O tripwire, with exact-content legacy exemptions.

Unlike a directory allowlist, an entry admits only the reviewed bytes of one
file. Every edit to existing direct ownership must shrink or explicitly review
that entry. This intentionally over-reports client-policy adapters during M0.
"""
import argparse
import hashlib
import json
import posixpath
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).with_name("baseline.json")
# These are module/operation identifiers, not substrings of comments or strings.
RUST_IO = {"fs", "process", "net", "env", "dirs", "libc", "include", "include_str", "include_bytes", "exists", "is_file", "is_dir", "metadata", "canonicalize", "read_dir", "read_to_string", "app_config_dir", "home_dir"}
RUST_HOST = {"skiff_core", "config", "ledger", "skipper", "agent_folder", "skiff_agent"}
JS_IO = {"invoke", "fetch", "XMLHttpRequest", "WebSocket", "require", "Deno", "Bun"}


def regex_can_start(lexed):
    # A run of ! operators preserves whether an operand was expected before
    # it: !/regex/ is prefix negation, but value! / divisor is TS postfix
    # non-null assertion followed by division. Comments are already skipped.
    i = len(lexed) - 1
    while i >= 0 and lexed[i] == ("punct", "!"):
        i -= 1
    return i < 0 or lexed[i][1] in {
        "(", "[", "{", "=", ":", ",", ";", "?", "&", "|", ">", "return", "throw",
    }


def regex_end(source, start):
    """Only consume a closed, single-line regex; otherwise keep scanning code."""
    i, character_class = start + 1, False
    while i < len(source):
        if source[i] in "\r\n\u2028\u2029": return None
        if source[i] == "\\":
            if i + 1 == len(source) or source[i + 1] in "\r\n\u2028\u2029": return None
            i += 2
            continue
        if source[i] == "[": character_class = True
        elif source[i] == "]": character_class = False
        elif source[i] == "/" and not character_class: return i + 1
        i += 1
    return None


def tokens(source, rust=False, _expression=False):
    """Keep string import specifiers; skip comments, including nested Rust ones."""
    out, i = [], 0
    braces = 0
    while i < len(source):
        if source[i].isspace():
            i += 1
        elif source.startswith("//", i):
            end = source.find("\n", i)
            i = len(source) if end < 0 else end + 1
        elif source.startswith("/*", i):
            depth, i = 1, i + 2
            while depth and i < len(source):
                if source.startswith("/*", i): depth, i = depth + 1, i + 2
                elif source.startswith("*/", i): depth, i = depth - 1, i + 2
                else: i += 1
        else:
            raw = re.match(r'r(#+)?"', source[i:]) if rust else None
            if raw:
                start = i + len(raw[0])
                end = source.find('"' + (raw[1] or ''), start)
                i = len(source) if end < 0 else end + 1 + len(raw[1] or '')
                out.append(("string", source[start:end]))
            elif (_expression and source[i] == "/" and regex_can_start(out)
                  and (end := regex_end(source, i)) is not None):
                # Regex braces/backticks are inert, but a failed regex guess
                # must never swallow the rest of an interpolation or file.
                i = end
                out.append(("regex", ""))
            elif not rust and source[i] == "`":
                # Literal chunks are inert; ${...} is executable code, including
                # nested templates, strings, comments and object/block braces.
                i += 1
                start = i
                while i < len(source) and source[i] != "`":
                    if source[i] == "\\":
                        i += 2
                    elif source.startswith("${", i):
                        out.append(("string", source[start:i]))
                        nested, consumed = tokens(source[i + 2:], _expression=True)
                        out.extend(nested)
                        i += 2 + consumed
                        start = i
                    else:
                        i += 1
                out.append(("string", source[start:i]))
                i += 1
            elif source[i] in '\"\'`' and not (rust and source[i] == "'" and not re.match(r"'(?:\\.|[^'\\])'", source[i:])):
                quote, start = source[i], i + 1
                i += 1
                while i < len(source) and source[i] != quote:
                    i += 2 if source[i] == "\\" else 1
                out.append(("string", source[start:i]))
                i += 1
            else:
                word = re.match(r"[A-Za-z_][A-Za-z_0-9]*", source[i:])
                if word:
                    out.append(("word", word[0])); i += len(word[0])
                else:
                    if _expression:
                        if source[i] == "}" and braces == 0:
                            return out, i + 1
                        if source[i] == "{": braces += 1
                        elif source[i] == "}": braces -= 1
                    out.append(("punct", source[i])); i += 1
    return (out, i) if _expression else out


def risks(source, suffix):
    found = set()
    ts = tokens(source, rust=suffix == ".rs")
    for i, (kind, value) in enumerate(ts):
        if suffix == ".rs" and kind == "word":
            if value in RUST_HOST or value in RUST_IO:
                found.add(value)
            if value == "skiff_client":
                # The new connection is the sanctioned seam; grouped/aliased
                # legacy imports remain frozen rather than guessing resolution.
                following = [v for _, v in ts[i + 1:i + 4]]
                if following != [":", ":", "host"]:
                    found.add(value)
        elif suffix != ".rs":
            if kind == "word" and value in JS_IO:
                found.add(value)
            if kind == "string" and (value.startswith(("@tauri-apps/", "node:", "fs", "child_process"))
                                     or value.endswith(("/platform/os", "/platform/fileActions"))):
                found.add(value)
    return sorted(found)


def rust_modules(lexed):
    modules = {}
    for file, ts in lexed.items():
        if file.suffix != ".rs": continue
        name = file.parent.name if file.stem == "mod" else file.stem
        if name not in {"lib", "main"}:
            modules.setdefault(name, set()).add(file)
        # Inline modules can re-export adapters just like file modules.
        for i in range(len(ts) - 2):
            if ts[i] == ("word", "mod") and ts[i + 2] == ("punct", "{"):
                modules.setdefault(ts[i + 1][1], set()).add(file)
    return modules


def local_edges(file, lexed, files, modules):
    """Conservative module-level edges, not symbol/type resolution.

    Follow literal TS paths (including barrels) and Rust local module names,
    including qualified/grouped uses and re-exports. Module declarations alone
    do not make their parent a caller. Ambiguous Rust names over-report safely.
    """
    edges = set()
    if file.suffix != ".rs":
        for kind, value in lexed:
            if kind != "string" or not value.startswith("."): continue
            path = Path(posixpath.normpath(str(file.parent / value)))
            candidates = [path]
            # Bundler resolution permits explicit .ts and emitted .js imports.
            base = path.with_suffix("") if path.suffix in {".js", ".jsx", ".mjs", ".cjs"} else path
            candidates += [Path(str(base) + ext) for ext in (".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs")]
            candidates += [path / ("index" + ext) for ext in (".ts", ".tsx", ".js", ".jsx")]
            edges.update(p for p in candidates if p in files)
    else:
        for i, (kind, value) in enumerate(lexed):
            if kind != "word": continue
            if i and lexed[i - 1] == ("word", "mod"): continue
            edges.update(modules.get(value, ()))
            # Root-level symbol re-exports need the crate root as their provider.
            if value in {"crate", "self", "super"} and [v for _, v in lexed[i + 1:i + 3]] == [":", ":"]:
                # Conservatively include ancestor modules too: a symbol may be
                # re-exported under the same name as a file module.
                edges.update(p for p in files if p.suffix == ".rs" and
                             (p.name == "lib.rs" or p.parent in file.parents and
                              p.name == "mod.rs"))
    return edges - {file}


def inventory(root):
    result, sources = {}, {}
    for folder in ["apps/desktop/src", "apps/desktop/src-tauri/src"]:
        for file in sorted((root / folder).rglob("*")):
            if file.suffix in {".rs", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"}:
                sources[file.relative_to(root)] = file.read_text()
    reasons = {p: risks(source, p.suffix) for p, source in sources.items()}
    lexed = {p: tokens(source, rust=p.suffix == ".rs") for p, source in sources.items()}
    modules = rust_modules(lexed)
    edges = {p: local_edges(p, ts, sources, modules) for p, ts in lexed.items()}
    # Fixed point includes callers of callers and barrel/re-export chains.
    flagged = {p for p, found in reasons.items() if found}
    while True:
        expanded = flagged | {p for p, deps in edges.items() if deps & flagged}
        if expanded == flagged: break
        flagged = expanded
    for file in sorted(flagged):
        providers = sorted(str(p) for p in edges[file] & flagged)
        result[str(file)] = {
            "sha256": hashlib.sha256((root / file).read_bytes()).hexdigest(),
            "reasons": reasons[file] or ["local provider: " + p for p in providers],
        }
    # A new dependency or alias must not introduce an uninspected I/O provider.
    for name in ["apps/desktop/src-tauri/Cargo.toml", "apps/desktop/package.json"]:
        result[name] = {"sha256": hashlib.sha256((root / name).read_bytes()).hexdigest(), "reasons": ["dependency surface"]}
    return result


def compare(actual, baseline):
    errors = []
    for file, entry in actual.items():
        old = baseline.get(file)
        if old is None or old["sha256"] != entry["sha256"]:
            errors.append(f"{file}: new/changed boundary access ({', '.join(entry['reasons'])}); use HostConnection or explicitly review the exact legacy baseline")
    for file in baseline.keys() - actual.keys():
        errors.append(f"{file}: obsolete exemption; remove it from the baseline")
    return errors


def refresh(baseline, actual, files):
    """Re-record the hash of named, already-reviewed entries; nothing else.

    A release bumps the version in a fingerprinted file. This admits those
    bytes without re-recording the rest of the inventory, so every other
    unreviewed change still fails the check. A file that is not already in
    the baseline is refused: a new entry needs a full, reviewed --record.
    """
    for file in files:
        if file not in baseline:
            raise SystemExit(f"{file}: not in the baseline; --refresh only updates an existing entry")
        if file not in actual:
            raise SystemExit(f"{file}: no longer in the inventory; remove it from the baseline")
    updated = dict(baseline)
    for file in files:
        updated[file] = {**baseline[file], "sha256": actual[file]["sha256"], "reasons": actual[file]["reasons"]}
    return updated


def api_errors(root):
    manifest = tomllib.loads((root / "crates/skiff-host-api/Cargo.toml").read_text())
    errors = []
    if manifest.get("dependencies", {}) != {"serde": {"workspace": True}, "base64": {"workspace": True}} or manifest.get("build-dependencies") or manifest.get("target"):
        errors.append("skiff-host-api: runtime dependencies must remain only serde/base64, with no build/target dependencies")
    for source in (root / "crates/skiff-host-api/src").rglob("*.rs"):
        forbidden = set(risks(source.read_text(), ".rs"))
        if forbidden: errors.append(f"{source}: I/O/application dependency in contract crate: {sorted(forbidden)}")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--record", action="store_true", help="explicitly review and replace the legacy inventory (never run in CI)")
    parser.add_argument("--refresh", action="append", default=[], metavar="FILE",
                        help="re-record the hash of one existing baseline entry, for example after a release bumps its version (repeatable)")
    args = parser.parse_args()
    actual = inventory(ROOT)
    if args.refresh:
        baseline = refresh(json.loads(BASELINE.read_text()), actual, args.refresh)
        BASELINE.write_text(json.dumps(baseline, indent=2, sort_keys=True) + "\n")
    if args.record:
        # Purpose labels make client-owned exceptions explicit and reviewable.
        old = json.loads(BASELINE.read_text()) if BASELINE.exists() else {}
        client_files = {
            "apps/desktop/src-tauri/src/clipboard.rs", "apps/desktop/src-tauri/src/header_bar.rs",
            "apps/desktop/src-tauri/src/native_menu.rs", "apps/desktop/src-tauri/src/quit.rs",
            "apps/desktop/src-tauri/src/workspace_file.rs", "apps/desktop/src/workspace/profile.ts",
            "apps/desktop/src/appearance/appTheme.ts", "apps/desktop/src/appearance/themeCards.ts",
            "apps/desktop/src/diagnostics/latency.ts", "apps/desktop/src/diagnostics/memory.ts",
            "apps/desktop/src/platform/clipboard.ts", "apps/desktop/src/terminal/terminalFont.ts",
            "apps/desktop/src/ui/infoDialogs.ts",

        }
        for file, entry in actual.items():
            client = file in client_files
            entry["ownership"] = old.get(file, {}).get("ownership", "client policy: exact existing adapter only" if client else "legacy local adapter/caller: migrate behind Host API in M1–M6")
        BASELINE.write_text(json.dumps(actual, indent=2, sort_keys=True) + "\n")
    baseline = json.loads(BASELINE.read_text())
    errors = compare(actual, baseline) + api_errors(ROOT)
    if errors:
        print("\n".join(errors)); return 1
    print(f"Host boundary passed: {len(baseline)} exact-content baseline entries; Host API dependencies remain I/O-free")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
