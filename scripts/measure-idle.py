#!/usr/bin/env python3
"""Measure a nested release build; CPU is percent of one core, memory is PSS.

Run from an existing desktop: python3 scripts/measure-idle.py --seconds 300.
No screenshot/readback or input replay runs during the measurement. Xwayland is reported
separately; the core total is navi + navi-bar. Resident and swapped PSS are separate.
Optional --windows 3 needs Konsole. Only processes started here are stopped.
"""
import argparse
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile
import time


def processes(roots):
    found, pending = {}, list(roots)
    while pending:
        pid = pending.pop()
        if pid in found:
            continue
        try:
            proc = Path(f"/proc/{pid}")
            stat = (proc / "stat").read_text().rsplit(")", 1)[1].split()
            name = (proc / "comm").read_text().strip()
            ticks = int(stat[11]) + int(stat[12])
            memory = {
                line.split()[0].rstrip(":"): int(line.split()[1])
                for line in (proc / "smaps_rollup").read_text().splitlines()[1:]
            }
            found[pid] = {"name": name, "ticks": ticks, "pss_kib": memory["Pss"], "swap_pss_kib": memory.get("SwapPss", 0)}
            for task in (proc / "task").iterdir():
                pending.extend(int(p) for p in (task / "children").read_text().split())
        except (FileNotFoundError, ProcessLookupError):
            continue
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/navi")
    parser.add_argument("--seconds", type=int, default=60)
    parser.add_argument("--warmup", type=int, default=10)
    parser.add_argument("--windows", type=int, choices=[0, 3], default=0, help="open three idle Konsole windows with cursor blinking disabled")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if not (os.environ.get("WAYLAND_DISPLAY") or os.environ.get("DISPLAY")):
        parser.error("run nested from an existing desktop")
    if args.seconds < 1 or args.warmup < 0:
        parser.error("seconds must be positive and warmup nonnegative")
    env = {k: v for k, v in os.environ.items() if k not in ("NAVI_SCREENSHOT", "NAVI_INPUT_REPLAY")}
    env["RUST_LOG"] = "warn,navi=info"
    with tempfile.TemporaryDirectory(prefix="navi-benchmark-") as state, tempfile.TemporaryFile(mode="w+") as log:
        env["XDG_STATE_HOME"] = state
        child = subprocess.Popen([str(Path(args.binary).resolve())], env=env, stdout=log, stderr=subprocess.STDOUT)
        apps = []
        roots = [child.pid]
        try:
            if args.windows:
                for _ in range(100):
                    time.sleep(0.1)
                    log.seek(0)
                    display = re.search(r"listening on WAYLAND_DISPLAY=(\S+)", log.read())
                    if display:
                        break
                else:
                    raise RuntimeError("Navi did not announce its nested display")
                app_env = dict(env, WAYLAND_DISPLAY=display[1], DISPLAY="", QT_QPA_PLATFORM="wayland")
                app_env.pop("WAYLAND_SOCKET", None)
                for _ in range(args.windows):
                    command = [
                        "konsole", "--separate", "--builtin-profile", "--hide-menubar",
                        "--hide-tabbar", "-p", "BlinkingCursorEnabled=false",
                        "-e", "sleep", str(args.seconds + args.warmup + 60),
                    ]
                    apps.append(subprocess.Popen(
                        command, env=app_env, stdout=log, stderr=subprocess.STDOUT))
                roots.extend(app.pid for app in apps)
            print(f"Navi PID {child.pid}; warming up {args.warmup}s", flush=True)
            time.sleep(args.warmup)
            before = processes(roots)
            start = time.monotonic()
            samples = []
            for second in range(args.seconds):
                time.sleep(1)
                if child.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(f"Navi exited {child.returncode}: {log.read()}")
                samples.append(processes(roots))
                if (second + 1) % 30 == 0:
                    print(f"Measured {second + 1}/{args.seconds}s", flush=True)
            elapsed = time.monotonic() - start
            if args.windows and any(app.pid not in before for app in apps):
                raise RuntimeError("A benchmark terminal failed to start")
            rows = []
            for pid, initial in before.items():
                history = [s[pid] for s in samples if pid in s]
                if len(history) != args.seconds:
                    raise RuntimeError(f"Process {pid} disappeared; repeat the benchmark")
                rows.append({
                    "pid": pid,
                    "name": initial["name"],
                    "cpu_percent_one_core": round(
                        (history[-1]["ticks"] - initial["ticks"])
                        / os.sysconf("SC_CLK_TCK") / elapsed * 100, 3),
                    "pss_mib_start": round(initial["pss_kib"] / 1024, 2),
                    "pss_mib_end": round(history[-1]["pss_kib"] / 1024, 2),
                    "pss_mib_peak": round(max(s["pss_kib"] for s in history) / 1024, 2),
                    "swap_pss_mib_end": round(history[-1]["swap_pss_kib"] / 1024, 2),
                    "pss_including_swap_mib_start": round(
                        (initial["pss_kib"] + initial["swap_pss_kib"]) / 1024, 2),
                    "pss_including_swap_mib_end": round(
                        (history[-1]["pss_kib"] + history[-1]["swap_pss_kib"]) / 1024, 2),
                })
            core = [r for r in rows if r["name"] in ("navi", "navi-bar")]
            if len(core) != 2:
                raise RuntimeError(f"Expected navi and navi-bar: {rows}")
            result = {
                "seconds": round(elapsed, 2),
                "windows": args.windows,
                "core_cpu_percent_one_core": round(sum(r["cpu_percent_one_core"] for r in core), 3),
                "core_pss_mib_end": round(sum(r["pss_mib_end"] for r in core), 2),
                "core_pss_including_swap_mib_start": round(
                    sum(r["pss_including_swap_mib_start"] for r in core), 2),
                "core_pss_including_swap_mib_end": round(
                    sum(r["pss_including_swap_mib_end"] for r in core), 2),
                "processes": rows,
            }
            text = json.dumps(result, indent=2)
            print(text, flush=True)
            if args.output:
                args.output.write_text(text + "\n")
        finally:
            for app in apps:
                if app.poll() is None:
                    app.terminate()
                    app.wait(timeout=5)
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=5)


if __name__ == "__main__":
    main()
