#!/usr/bin/env python3
"""Restore KiCad 10 PCB syntax after Atopile 0.15.x updates the board."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

PCB_PATH = Path(__file__).parent / "layouts" / "default" / "default.kicad_pcb"
UUID_LOOKAHEAD_LINES = 12
NET_NAME_LOOKAHEAD_LINES = 4


def _decode_net(raw_name: str) -> str:
    return bytes(raw_name, "utf-8").decode("unicode_escape")


def _quote_net(name: str) -> str:
    return '"' + name.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _read_net_table(content: str) -> dict[int, str]:
    nets: dict[int, str] = {0: ""}
    for match in re.finditer(r'^\t\(net\s+(\d+)\s+"((?:[^"\\]|\\.)*)"\)\n?', content, re.M):
        nets[int(match.group(1))] = _decode_net(match.group(2))
    return nets


def _nearby_uuid(lines: list[str], line_index: int) -> str | None:
    stop = min(len(lines), line_index + UUID_LOOKAHEAD_LINES)
    for index in range(line_index, stop):
        match = re.search(r'\(uuid\s+"([^"]+)"\)', lines[index])
        if match:
            return match.group(1)
    return None


def _nearby_net_name(lines: list[str], line_index: int) -> str | None:
    stop = min(len(lines), line_index + NET_NAME_LOOKAHEAD_LINES)
    for index in range(line_index, stop):
        match = re.search(r'\(net_name\s+"((?:[^"\\]|\\.)*)"\)', lines[index])
        if match:
            return _decode_net(match.group(1))
    return None


def _read_uuid_net_references(content: str) -> dict[str, str]:
    references: dict[str, str] = {}
    lines = content.splitlines()

    for index, line in enumerate(lines):
        match = re.search(r'\(net\s+"((?:[^"\\]|\\.)*)"\)', line)
        if not match:
            continue

        uuid = _nearby_uuid(lines, index)
        if uuid:
            references[uuid] = _decode_net(match.group(1))

    return references


def _read_git_reference(revision: str) -> str:
    result = subprocess.run(
        ["git", "show", revision],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout


def _restore_via_protection_setup(content: str) -> tuple[str, int]:
    pattern = re.compile(r'(?P<indent>\t+)\(tenting(?P<layers>(?:\s+(?:front|back))*)\)')

    def replacement(match: re.Match[str]) -> str:
        layers = set(match.group("layers").split())
        indent = match.group("indent")
        front = "yes" if "front" in layers else "no"
        back = "yes" if "back" in layers else "no"
        return (
            f"{indent}(tenting\n"
            f"{indent}\t(front {front})\n"
            f"{indent}\t(back {back})\n"
            f"{indent})\n"
            f"{indent}(covering\n"
            f"{indent}\t(front no)\n"
            f"{indent}\t(back no)\n"
            f"{indent})\n"
            f"{indent}(plugging\n"
            f"{indent}\t(front no)\n"
            f"{indent}\t(back no)\n"
            f"{indent})\n"
            f"{indent}(capping no)\n"
            f"{indent}(filling no)"
        )

    return pattern.subn(replacement, content)


def _remove_net_table(content: str) -> tuple[str, int]:
    return re.subn(r'^\t\(net\s+\d+\s+"(?:[^"\\]|\\.)*"\)\n?', "", content, flags=re.M)


def _restore_name_only_net_references(
    content: str,
    nets: dict[int, str],
    uuid_net_references: dict[str, str] | None = None,
) -> tuple[str, int]:
    uuid_net_references = uuid_net_references or {}
    unresolved: list[str] = []
    replacements = 0
    restored_lines: list[str] = []
    lines = content.splitlines(keepends=True)

    for index, line in enumerate(lines):
        def replacement(match: re.Match[str]) -> str:
            net_id = int(match.group(1))
            if net_id in nets:
                return f"(net {_quote_net(nets[net_id])})"

            net_name = _nearby_net_name(lines, index)
            if net_name is not None:
                return f"(net {_quote_net(net_name)})"

            uuid = _nearby_uuid(lines, index)
            if uuid and uuid in uuid_net_references:
                return f"(net {_quote_net(uuid_net_references[uuid])})"

            unresolved.append(f"line {index + 1}: net {net_id}, uuid {uuid or 'unknown'}")
            return match.group(0)

        updated, count = re.subn(r'\(net\s+(\d+)\)', replacement, line)
        replacements += count
        restored_lines.append(updated)

    if unresolved:
        details = "\n".join(unresolved[:25])
        extra = "" if len(unresolved) <= 25 else f"\n... and {len(unresolved) - 25} more"
        raise RuntimeError(f"Could not resolve all numbered net references:\n{details}{extra}")

    return "".join(restored_lines), replacements


def restore_pcb(
    pcb_path: Path,
    reference_content: str | None = None,
) -> tuple[int, int, int]:
    content = pcb_path.read_text()
    nets = _read_net_table(content)
    uuid_net_references = _read_uuid_net_references(reference_content) if reference_content else {}

    content, via_setup_changes = _restore_via_protection_setup(content)
    content, net_table_removals = _remove_net_table(content)
    content, net_reference_changes = _restore_name_only_net_references(content, nets, uuid_net_references)

    if via_setup_changes or net_table_removals or net_reference_changes:
        pcb_path.write_text(content)

    return via_setup_changes, net_table_removals, net_reference_changes


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("pcb", nargs="?", type=Path, default=PCB_PATH)
    parser.add_argument("--reference", type=Path)
    parser.add_argument("--reference-git")
    args = parser.parse_args()

    pcb = args.pcb
    if not pcb.exists():
        print(f"PCB file not found: {pcb}", file=sys.stderr)
        sys.exit(1)

    reference_content = None
    if args.reference:
        reference_content = args.reference.read_text()
    elif args.reference_git:
        reference_content = _read_git_reference(args.reference_git)

    via_setup_changes, net_table_removals, net_reference_changes = restore_pcb(pcb, reference_content)
    print(
        "Restored KiCad 10 syntax after Atopile: "
        f"{via_setup_changes} via setup block(s), "
        f"{net_table_removals} net table entry/entries, "
        f"{net_reference_changes} numbered net reference(s)"
    )
