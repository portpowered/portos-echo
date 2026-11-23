#!/usr/bin/python3
"""Local fixture for Alexa, Android settings, the screen package and RTC."""

import json
import sys
from pathlib import Path

tool = Path(sys.argv[0])
root = tool.parent.parent
args = sys.argv[1:]
stage = args[0]
state_file = root / "device.json"
state = json.loads(state_file.read_text())
packages = state["packages"]
settings = state["settings"]
installed = root / "installed/base.apk"
with (root / "device-commands").open("a") as transcript:
    print((root / "time").read_text(), tool.name, *args, file=transcript)

if (root / "fail").exists() and (root / "fail").read_text().split() == [tool.name, *args]:
    print("fixture raw diagnostic")
    sys.exit(7)

if tool.name == "dumpsys":
    assert args == ["package", args[-1]]
    print(f"  User 0: installed=true enabled={packages[args[-1]]}")
elif tool.name == "pm":
    if stage == "list":
        assert args[:4] == ["list", "packages", "--user", "0"]
        if args[4:] == ["-e"]:
            for pkg, enabled in packages.items():
                if enabled in (0, 1):
                    print("package:" + pkg)
        else:
            assert args[4:] == ["org.portos.echoscreen"]
            if installed.exists():
                print("package:org.portos.echoscreen")
    elif stage == "path":
        assert args[1:] == ["--user", "0", "org.portos.echoscreen"]
        assert installed.exists()
        print("package:" + str(installed))
        print("package:" + str(installed.with_name("split_config.apk")))
    elif stage == "install":
        assert args[1] == "-r"
        candidate = Path(args[2]).read_bytes()
        if not candidate.startswith(b"fixture-apk:"):
            print("Failure [invalid fixture APK]")
        else:
            installed.parent.mkdir(exist_ok=True)
            installed.write_bytes(candidate)
            print("Success")
    else:
        assert args[1:3] == ["--user", "0"]
        packages[args[-1]] = {"disable-user": 3, "default-state": 0, "enable": 1}[stage]
elif tool.name == "settings":
    assert args[:2] == ["--user", "0"]
    action, namespace, *fields = args[2:]
    values = settings.setdefault(namespace, {})
    if action == "get":
        print(values.get(fields[0], "null"))
    elif action == "list":
        for key, value in values.items():
            print(f"{key}={value}")
    elif action == "put":
        values[fields[0]] = fields[1]
    else:
        assert action == "delete"
        values.pop(fields[0], None)
elif tool.name == "am":
    if stage == "broadcast":
        assert args[1:-1] == [
            "--user", "0", "-a",
            "com.amazon.android.service.wifidiags.SET_ENABLEWIFIDIAGS",
            "--ez", "val",
        ]
        assert args[-1] in ("true", "false")
        print("Broadcast completed: result=0")
    else:
        assert args == ["start", "-W", "--user", "0", "-n", "org.portos.echoscreen/.MainActivity"]
        assert installed.exists()
        print("Status: ok")
else:
    assert tool.name == "hwclock" and args == ["-w", "-u"]

state_file.write_text(json.dumps(state))
