#!/usr/bin/env python3
"""Development-only macOS/Linux RSS samples; synthetic config, no device I/O."""
import argparse
import json
from pathlib import Path
import select
import subprocess
import tempfile


def run(binary, flags, iterations):
    process = subprocess.Popen(
        [str(binary), *flags, "serve"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        def rpc(method, params=None):
            request = {"jsonrpc": "2.0", "id": 1, "method": method}
            if params is not None:
                request["params"] = params
            process.stdin.write(json.dumps(request) + "\n")
            process.stdin.flush()
            if not select.select([process.stdout], [], [], 10)[0]:
                raise RuntimeError("Timed out waiting for MCP response")
            line = process.stdout.readline()
            if not line:
                raise RuntimeError(process.stderr.read())
            if "result" not in json.loads(line):
                raise RuntimeError("Unexpected MCP response")

        def rss():
            return int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(process.pid)]))

        rpc("initialize", {"protocolVersion": "2025-11-25"})
        samples = [rss()]
        for i in range(iterations):
            rpc("tools/list")
            if i % 100 == 99 or i == iterations - 1:
                samples.append(rss())
        process.stdin.close()
        process.wait(timeout=5)
        if process.returncode:
            raise RuntimeError(process.stderr.read())
        return {"idle_kib": samples[0], "max_sampled_kib": max(samples), "final_kib": samples[-1]}
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        for pipe in (process.stdin, process.stdout, process.stderr):
            pipe.close()


def main():
    repo = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=repo / "target/release/homewire")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--iterations", type=int, default=1000)
    parser.add_argument("--media", action="store_true", help="Include Samsung/DIGI with synthetic credentials; no network I/O")
    parser.add_argument("--legacy-tools", action="store_true", help="Measure the compatibility MCP tool catalog")
    args = parser.parse_args()
    if not 1 <= args.runs <= 10 or not 1 <= args.iterations <= 10000:
        parser.error("runs must be 1–10; iterations must be 1–10000")
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="homewire-memory-") as temp:
        flags = []
        for flag, file in [("config", "meaco"), ("printers", "printers"), ("awair", "awair"), ("lights", "lights"), ("plugs", "plugs")]:
            value = json.loads((repo / "examples" / (file + ".json")).read_text())
            value.pop("automation", None)
            if file == "meaco":
                value["local_key"] = "0123456789abcdef"
            for key in ("lights", "plugs"):
                for endpoint in value.get(key, []):
                    endpoint["auth_hash"] = "00" * 32
            path = Path(temp) / (file + ".json")
            path.write_text(json.dumps(value))
            path.chmod(0o600)
            flags.extend(["--" + flag, str(path)])
        if args.media:
            for name in ("cert", "key"):
                source = repo / "tests" / "fixtures" / "media" / ("test-" + name + ".pem")
                path = Path(temp) / ("digi-" + name + ".pem")
                path.write_bytes(source.read_bytes())
                path.chmod(0o600)
            for name in ("samsung", "digi"):
                value = json.loads((repo / "examples" / (name + ".json")).read_text())
                if name == "samsung":
                    value["token"] = "00" * 16
                else:
                    value["server_cert_sha256"] = "00" * 32
                path = Path(temp) / (name + ".json")
                path.write_text(json.dumps(value))
                path.chmod(0o600)
                flags.extend(["--" + name, str(path)])
        if args.legacy_tools:
            flags.append("--legacy-tools")
        trials = [run(binary, flags, args.iterations) for _ in range(args.runs)]
    print(json.dumps({"binary_bytes": binary.stat().st_size, "trials": trials}))


if __name__ == "__main__":
    main()
