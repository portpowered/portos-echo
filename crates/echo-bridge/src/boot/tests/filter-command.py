#!/usr/bin/python3
"""Fixture for filter inventory, atomic restore and Android adb properties."""

import sys
from pathlib import Path

tool = Path(sys.argv[0])
root = tool.parent.parent
name = tool.name
args = sys.argv[1:]
time = (root / "time").read_text()

with (root / "commands").open("a") as transcript:
    print(time, name, *args, file=transcript)


def restore(table, listing, commands):
    assert commands[0] == "*filter" and commands[-1] == "COMMIT"
    for command in commands[1:-1]:
        op, chain, *spec = command.split()
        rule = " ".join(["-A", chain, *spec])
        if op == ":echo_out":
            listing = [line for line in listing if not line.startswith("-A echo_out ")]
            if "-N echo_out" not in listing:
                listing.insert(2, "-N echo_out")
        elif op == "-P":
            i = next(i for i, line in enumerate(listing) if line.startswith(f"-P {chain} "))
            listing[i] = command
        elif op == "-A":
            listing.append(command)
        elif op == "-D":
            listing.remove(rule)
        else:
            assert op == "-I" and spec[0] == "1"
            i = next(
                (i for i, line in enumerate(listing) if line.startswith(f"-A {chain} ")),
                3,
            )
            listing.insert(i, " ".join(["-A", chain, *spec[1:]]))
    table.write_text("\n".join(listing) + "\n")


if name == "getprop":
    if not args:
        if (root / "resolvers.fail").exists():
            sys.exit(7)
        sys.stdout.write((root / "resolvers").read_text())
    elif args == ["service.adb.tcp.port"]:
        sys.stdout.write((root / "adb-port").read_text())
    else:
        assert args == ["init.svc.adbd"]
        print("running")
elif name == "setprop":
    if args == ["service.adb.tcp.port", "0"]:
        (root / "adb-port").write_text("0")
    else:
        assert args == ["ctl.restart", "adbd"]
        (root / "proc/net/tcp").write_text("")
        (root / "proc/net/tcp6").write_text("")
else:
    if (root / (name + ".fail")).exists():
        sys.exit(7)
    table = root / name.removesuffix("-restore")
    if name.endswith("-restore"):
        assert args == ["-w", "--noflush"]
        restore(table, table.read_text().splitlines(), sys.stdin.read().splitlines())
    else:
        assert args == ["-w", "-S"]
        sys.stdout.write(table.read_text())
