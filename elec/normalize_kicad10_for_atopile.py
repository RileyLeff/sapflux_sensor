#!/usr/bin/env python3
"""Normalize KiCad 10 PCB syntax that Atopile 0.15.x cannot parse yet."""

from __future__ import annotations

import re
import sys
from pathlib import Path

PCB_PATH = Path(__file__).parent / "layouts" / "default" / "default.kicad_pcb"


def _quote_net(name: str) -> str:
    return '"' + name.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _normalize_via_protection_setup(content: str) -> tuple[str, int]:
    pattern = re.compile(
        r"""
        (?P<indent>\t+)\(tenting\s*
            \n(?P=indent)\t\(front\s+(?P<front>yes|no)\)
            \n(?P=indent)\t\(back\s+(?P<back>yes|no)\)
            \n(?P=indent)\)
        \n(?P=indent)\(covering\s*
            \n(?P=indent)\t\(front\s+(?:yes|no)\)
            \n(?P=indent)\t\(back\s+(?:yes|no)\)
            \n(?P=indent)\)
        \n(?P=indent)\(plugging\s*
            \n(?P=indent)\t\(front\s+(?:yes|no)\)
            \n(?P=indent)\t\(back\s+(?:yes|no)\)
            \n(?P=indent)\)
        \n(?P=indent)\(capping\s+(?:yes|no)\)
        \n(?P=indent)\(filling\s+(?:yes|no)\)
        """,
        re.VERBOSE,
    )

    def replacement(match: re.Match[str]) -> str:
        layers = []
        if match.group("front") == "yes":
            layers.append("front")
        if match.group("back") == "yes":
            layers.append("back")
        return f'{match.group("indent")}(tenting {" ".join(layers)})'

    return pattern.subn(replacement, content)


def _find_net_names(content: str) -> list[str]:
    names: list[str] = []
    seen: set[str] = set()
    for match in re.finditer(r'\(net\s+"((?:[^"\\]|\\.)*)"\)', content):
        name = bytes(match.group(1), "utf-8").decode("unicode_escape")
        if name not in seen:
            names.append(name)
            seen.add(name)
    return names


def _read_existing_net_table(content: str) -> dict[str, int]:
    nets: dict[str, int] = {}
    for match in re.finditer(r'\n\t\(net\s+(\d+)\s+"((?:[^"\\]|\\.)*)"\)', content):
        name = bytes(match.group(2), "utf-8").decode("unicode_escape")
        nets[name] = int(match.group(1))
    return nets


def _read_existing_net_ids(content: str) -> dict[int, str]:
    return {net_id: name for name, net_id in _read_existing_net_table(content).items()}


def _normalize_name_only_nets(content: str) -> tuple[str, int]:
    net_names = _find_net_names(content)
    if not net_names:
        return content, 0

    net_ids = _read_existing_net_table(content)
    if not net_ids:
        net_ids = {name: i + 1 for i, name in enumerate(net_names)}
        declarations = "\n\t(net 0 \"\")"
        declarations += "".join(
            f"\n\t(net {net_id} {_quote_net(name)})"
            for name, net_id in sorted(net_ids.items(), key=lambda item: item[1])
        )

        first_footprint = content.find("\n\t(footprint ")
        if first_footprint < 0:
            raise RuntimeError("Could not find first footprint for net table insertion")
        content = content[:first_footprint] + declarations + content[first_footprint:]

    def replacement(match: re.Match[str]) -> str:
        raw_name = match.group(1)
        name = bytes(raw_name, "utf-8").decode("unicode_escape")
        net_id = net_ids[name]
        return f"(net {net_id})"

    content, replacements = re.subn(r'\(net\s+"((?:[^"\\]|\\.)*)"\)', replacement, content)
    return content, replacements


def _normalize_numbered_net_references(content: str) -> tuple[str, int]:
    replacements = 0
    lines: list[str] = []

    for line in content.splitlines(keepends=True):
        # Top-level net table entries are the only places that should keep the
        # human-readable net name in KiCad 9 syntax.
        if re.match(r'^\t\(net\s+\d+\s+"(?:[^"\\]|\\.)*"\)\n?$', line):
            lines.append(line)
            continue

        updated, count = re.subn(
            r'\(net\s+(\d+)\s+"(?:[^"\\]|\\.)*"\)',
            r"(net \1)",
            line,
        )
        lines.append(updated)
        replacements += count

    return "".join(lines), replacements


def _normalize_zone_net_names(content: str) -> tuple[str, int]:
    net_names_by_id = _read_existing_net_ids(content)

    def replacement(match: re.Match[str]) -> str:
        net_id = int(match.group("net_id"))
        net_name = net_names_by_id.get(net_id, "")
        return match.group(0) + f'\t\t(net_name {_quote_net(net_name)})\n'

    return re.subn(
        r'\n\t\(zone\s*\n\t\t\(net\s+(?P<net_id>\d+)\)\n(?!\t\t\(net_name\s)',
        replacement,
        content,
    )


def normalize_pcb(pcb_path: Path) -> tuple[int, int, int, int]:
    content = pcb_path.read_text()
    content, via_setup_changes = _normalize_via_protection_setup(content)
    content, net_changes = _normalize_name_only_nets(content)
    content, numbered_net_changes = _normalize_numbered_net_references(content)
    content, zone_net_name_changes = _normalize_zone_net_names(content)

    if via_setup_changes or net_changes or numbered_net_changes or zone_net_name_changes:
        pcb_path.write_text(content)

    return via_setup_changes, net_changes, numbered_net_changes, zone_net_name_changes


if __name__ == "__main__":
    pcb = Path(sys.argv[1]) if len(sys.argv) > 1 else PCB_PATH
    if not pcb.exists():
        print(f"PCB file not found: {pcb}", file=sys.stderr)
        sys.exit(1)

    via_setup_changes, net_changes, numbered_net_changes, zone_net_name_changes = normalize_pcb(pcb)
    print(
        "Normalized KiCad syntax for Atopile: "
        f"{via_setup_changes} via setup block(s), "
        f"{net_changes} name-only net reference(s), "
        f"{numbered_net_changes} named numbered net reference(s), "
        f"{zone_net_name_changes} zone net name(s)"
    )
