#!/usr/bin/env python3
"""Install a persistent MMX systemd user service without restarting processes."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile


def quote(value, command=False):
    if any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise ValueError("unit values must not contain control characters")
    value = value.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%")
    if command:
        value = value.replace("$", "$$")
    return '"' + value + '"'


def literal(value):
    # Single-value settings do not remove quotes (unlike ExecStart/Environment).
    # Spaces, quotes, backslashes and dollars are already literal here.
    quote(value)
    if value != value.strip():
        raise ValueError("single-value settings must not start or end with whitespace")
    return value.replace("%", "%%")


def unit_name(value):
    if not re.fullmatch(r"mmx-rust-[A-Za-z0-9_.@-]+", value):
        raise ValueError("service names must start with mmx-rust- and contain no paths")
    return value if value.endswith(".service") else value + ".service"


def render(args):
    directory = Path(args.working_directory).expanduser().resolve(strict=True)
    if not directory.is_dir():
        raise ValueError("working directory is not a directory")
    command = args.command
    if command and command[0] == "--":
        command = command[1:]
    if not command or not Path(command[0]).is_absolute():
        raise ValueError("provide an absolute executable path after --")
    if not Path(command[0]).is_file() or not os.access(command[0], os.X_OK):
        raise ValueError("command is not an executable file")
    lines = ["[Unit]", "Description=" + literal(args.description or args.name)]
    for key, values in [("After", args.after), ("Wants", args.wants)]:
        if values:
            if not all(re.fullmatch(r"[A-Za-z0-9_.@:-]+", v) for v in values):
                raise ValueError("invalid dependency unit name")
            lines.append(key + "=" + " ".join(values))
    lines += [
        "StartLimitIntervalSec=0",
        "",
        "[Service]",
        "Type=simple",
        "WorkingDirectory=" + literal(str(directory)),
    ]
    for mount in args.require_mount:
        if not Path(mount).is_absolute():
            raise ValueError("required mount paths must be absolute")
        lines.append("ExecStartPre=" + " ".join(quote(v, command=True) for v in ["/usr/bin/mountpoint", "--quiet", mount]))
    for path, minimum in args.require_free:
        if not Path(path).is_absolute() or int(minimum) < 0:
            raise ValueError("free-space checks need an absolute path and nonnegative bytes")
        helper = Path(__file__).resolve().with_name("check_storage.py")
        if not helper.is_file():
            raise ValueError("check_storage.py must be installed beside the installer")
        lines.append("ExecStartPre=" + " ".join(quote(v, command=True) for v in ["/usr/bin/python3", str(helper), path, str(int(minimum))]))
    lines += [
        "ExecStart=" + " ".join(quote(v, command=True) for v in command),
        "Restart=always",
        "RestartSec=10",
        "KillSignal=SIGINT",
        "TimeoutStopSec=" + str(args.stop_timeout),
        "Nice=" + str(args.nice),
        "IOSchedulingClass=idle",
        "UMask=0077",
        "LimitNOFILE=65536",
        "NoNewPrivileges=true",
        "StandardOutput=journal",
        "StandardError=journal",
    ]
    for value in args.environment:
        key, separator, _ = value.partition("=")
        if not separator or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key):
            raise ValueError("environment must be KEY=VALUE")
        lines.append("Environment=" + quote(value))
    return "\n".join(lines + ["", "[Install]", "WantedBy=default.target", ""])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--name", required=True, help="e.g. mmx-rust-harvester")
    parser.add_argument("--working-directory", required=True)
    parser.add_argument("--description")
    parser.add_argument("--after", action="append", default=[])
    parser.add_argument("--wants", action="append", default=[])
    parser.add_argument("--require-mount", action="append", default=[], help="retry startup until this filesystem is mounted")
    parser.add_argument("--require-free", action="append", nargs=2, default=[], metavar=("PATH", "BYTES"), help="check available space before startup")
    parser.add_argument("--environment", action="append", default=[])
    parser.add_argument("--nice", type=int, choices=range(-20, 20), default=10)
    parser.add_argument("--stop-timeout", type=int, default=180)
    parser.add_argument("--replace", action="store_true")
    parser.add_argument("--enable", action="store_true", help="enable at user-manager startup")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    try:
        name = unit_name(args.name)
        if args.stop_timeout < 1:
            raise ValueError("stop timeout must be positive")
        content = render(args)
        config = Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config")))
        if not config.is_absolute():
            raise ValueError("XDG_CONFIG_HOME must be absolute")
        destination = config / "systemd/user" / name
        if destination.exists() and destination.read_text() != content and not args.replace:
            raise ValueError("unit already exists; use --replace to update it")
        with tempfile.TemporaryDirectory(prefix="mmx-systemd-") as temporary:
            check = Path(temporary) / name
            check.write_text(content)
            subprocess.run(["systemd-analyze", "--user", "verify", str(check)], check=True)
        destination.parent.mkdir(parents=True, exist_ok=True)
        if not destination.exists() or destination.read_text() != content:
            with tempfile.NamedTemporaryFile(mode="w", dir=destination.parent, delete=False) as out:
                temporary = Path(out.name)
                try:
                    out.write(content)
                    out.flush()
                    os.fchmod(out.fileno(), 0o644)
                    os.fsync(out.fileno())
                    os.replace(temporary, destination)
                finally:
                    temporary.unlink(missing_ok=True)
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
        if args.enable:
            subprocess.run(["systemctl", "--user", "enable", str(destination)], check=True)
        print("Installed " + str(destination))
        print("Start: systemctl --user start " + name)
        print("Logs: journalctl --user -u " + name + " -f")
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, str(error) + "\n")


if __name__ == "__main__":
    main()
