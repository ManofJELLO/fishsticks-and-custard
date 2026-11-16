#!/usr/bin/env python3
"""Pre-release check: has anything this app depends on changed since the
last release was tested?

Run from the repository root before tagging a release:

    packaging/release/check.py            # check everything
    packaging/release/check.py --live     # also rerun every agent's real tests
    packaging/release/check.py --record   # after a clean run, save what was tested

What it looks at:
  agents    claude, codex-acp, codex, agy, opencode: the installed version
            against the one last tested (tested.json) and the newest upstream.
            A changed version reruns that agent's real end-to-end tests (they
            talk to the real agent, so they use a little of its quota).
  packages  every name in the PKGBUILD's depends/makedepends/optdepends still
            exists in the Arch repos or the AUR.
  crates    newer versions of the Rust dependencies: compatible ones (cargo
            update) and ones that need a Cargo.toml change.
  models    the default speech models still download, and whether the files
            changed upstream (their ETag).
  actions   GitHub Actions used by CI against their newest major version.
  build     cargo test and clippy.

Exit status 1 if anything FAILs. WARNs are worth reading but don't block.
Needs: curl, cargo, pacman. Optional: gh (actions check).
"""

import argparse
import json
import re
import subprocess
import sys
import time
import urllib.request
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TESTED = Path(__file__).resolve().parent / "tested.json"
PKGBUILD = ROOT / "packaging/arch/PKGBUILD"
WORKFLOWS = ROOT / ".github/workflows"
UA = "fsc-release-check"

# name, command, how to find the newest upstream version, real tests to run.
AGENTS = [
    ("claude", ["claude", "--version"], ("npm", "@anthropic-ai/claude-code"),
     ["agent::claude::real::real_claude_round_trip"]),
    ("codex-acp", ["codex-acp", "--version"], ("npm", "@agentclientprotocol/codex-acp"),
     ["agent::acp::real::real_acp_codex", "agent::acp::real::real_acp_codex_asks"]),
    ("codex", ["codex", "--version"], ("npm", "@openai/codex"),
     ["agent::acp::real::real_acp_codex", "agent::acp::real::real_acp_codex_asks"]),
    ("agy", ["agy", "--version"], ("aur", "antigravity-cli"),
     ["agent::acp::real::real_acp_antigravity", "agent::acp::real::real_acp_antigravity_asks"]),
    ("opencode", ["opencode", "--version"], ("arch", "opencode"),
     ["agent::acp::real::real_acp_opencode", "agent::acp::real::real_acp_opencode_asks"]),
]

results = []  # (level, section, text)


def report(level, section, text):
    results.append((level, section, text))
    print(f"  {level:<5} {text}", flush=True)


def run(cmd, timeout=600, **kw):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, cwd=ROOT, **kw)
        return p.returncode, p.stdout + p.stderr
    except FileNotFoundError:
        return 127, f"{cmd[0]}: not found"
    except subprocess.TimeoutExpired:
        return 124, f"{' '.join(cmd)}: timed out after {timeout}s"


def get(url, timeout=20):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read().decode()


def version_of(text):
    m = re.search(r"\d+(?:\.\d+)+", text)
    return m.group(0) if m else None


def vkey(v):
    return tuple(int(x) for x in re.findall(r"\d+", v or "0"))


def upstream(kind, name):
    try:
        if kind == "npm":
            return json.loads(get(f"https://registry.npmjs.org/{name}/latest"))["version"]
        if kind == "aur":
            r = json.loads(get(f"https://aur.archlinux.org/rpc/v5/info?arg[]={name}"))["results"]
            return version_of(r[0]["Version"].split("_")[0]) if r else None
        if kind == "arch":
            r = json.loads(get(f"https://archlinux.org/packages/search/json/?name={name}"))["results"]
            return r[0]["pkgver"] if r else None
    except Exception as e:  # network trouble shouldn't stop the rest
        return f"? ({e})"
    return None


def section(title):
    print(f"\n{title}", flush=True)


def check_agents(tested, live, now):
    section("Agents")
    ran = {}  # test name -> passed
    for name, cmd, (kind, pkg), tests in AGENTS:
        code, out = run(cmd, timeout=30)
        have = version_of(out) if code == 0 else None
        latest = upstream(kind, pkg)
        was = tested.get("agents", {}).get(name)
        if have is None:
            report("WARN", "agents", f"{name}: not installed, so not tested (newest: {latest})")
            continue
        now["agents"][name] = have
        note = f"{name} {have} (tested {was or 'never'}, newest {latest})"
        if latest and not latest.startswith("?") and vkey(latest) > vkey(have):
            report("WARN", "agents", f"{note}: update it and rerun, users will get {latest}")
        if have == was and not live:
            report("OK", "agents", note)
            continue
        failed = []
        for t in tests:
            if t not in ran:
                print(f"        running {t} ...", flush=True)
                code, out = run(["cargo", "test", "-q", "-p", "fsc-core", "--lib", "--", "--ignored", "--exact",
                                 "--test-threads=1", t], timeout=600)
                ran[t] = code == 0
                if code != 0:
                    print("\n".join("        | " + line for line in out.strip().splitlines()[-15:]))
            if not ran[t]:
                failed.append(t.rsplit("::", 1)[-1])
        why = "changed" if have != was else "--live"
        if failed:
            report("FAIL", "agents", f"{note}: {why}, and {', '.join(failed)} failed")
        else:
            report("OK", "agents", f"{note}: {why}, real tests pass")


def check_packages():
    section("Arch packages (PKGBUILD)")
    script = f"source '{PKGBUILD}'; for d in \"${{depends[@]}}\" \"${{makedepends[@]}}\" \"${{optdepends[@]}}\"; " \
             "do echo \"${d%%:*}\"; done"
    code, out = run(["bash", "-c", script])
    names = [re.split(r"[<>=]", n)[0].strip() for n in out.split() if n.strip()]
    missing = []
    for n in names:
        # -Sp resolves "provides" too (cargo is provided by rust).
        if run(["pacman", "-Sp", "--print-format", "%n", n], timeout=30)[0] == 0:
            continue
        try:
            aur = json.loads(get(f"https://aur.archlinux.org/rpc/v5/info?arg[]={n}"))["resultcount"]
        except Exception:
            aur = 0
        if not aur:
            missing.append(n)
    if missing:
        report("FAIL", "packages", f"not in the repos or the AUR: {', '.join(missing)} "
               "(renamed or dropped? `sudo pacman -Sy` first if the sync db is old)")
    else:
        report("OK", "packages", f"all {len(names)} found")


def check_crates():
    section("Rust crates")
    code, out = run(["cargo", "update", "--dry-run", "--verbose"], timeout=300)
    updates = [line.strip() for line in out.splitlines() if line.strip().startswith("Updating ")
               and "crates.io index" not in line]
    if updates:
        report("NOTE", "crates", f"{len(updates)} compatible updates (`cargo update`, then test):")
        for u in updates:
            print(f"          {u}")
    else:
        report("OK", "crates", "Cargo.lock is up to date")

    code, out = run(["cargo", "metadata", "--no-deps", "--format-version", "1"])
    deps = {}
    for p in json.loads(out)["packages"]:
        for d in p["dependencies"]:
            if d.get("source", "") and "crates.io" in d["source"]:
                deps.setdefault(d["rename"] or d["name"], (d["name"], d["req"]))
    behind = []
    for name, req in sorted(deps.values()):
        latest = newest_crate(name)
        if latest and not compatible(req, latest):
            behind.append(f"{name} {req} -> {latest}")
    if behind:
        report("WARN", "crates", "newer versions need a Cargo.toml change: " + ", ".join(behind))
    else:
        report("OK", "crates", f"all {len(deps)} direct dependencies allow the newest version")


def newest_crate(name):
    n = name.lower()
    prefix = {1: "1", 2: "2", 3: f"3/{n[0]}"}.get(len(n), f"{n[:2]}/{n[2:4]}")
    try:
        lines = get(f"https://index.crates.io/{prefix}/{n}").splitlines()
    except Exception:
        return None
    vs = [json.loads(line) for line in lines if line.strip()]
    # Build metadata ("1.0.3+spec-1.1.0") isn't a pre-release; drop it first.
    stable = [v["vers"].split("+")[0] for v in vs if not v.get("yanked") and "-" not in v["vers"].split("+")[0]]
    return max(stable, key=vkey) if stable else None


def compatible(req, v):
    """Whether cargo's default (caret) requirement `req` accepts `v`."""
    req = req.lstrip("^= ").split(",")[0].strip()
    if not req or req.startswith(("<", ">", "~", "*")):
        return True
    r, have = vkey(req), vkey(v)
    for i, part in enumerate(r):
        if part != 0 or i == len(r) - 1:
            return have[: i + 1] == r[: i + 1] and have >= r
    return True


def check_models(tested, now):
    section("Speech models (default settings)")
    run(["cargo", "build", "-q", "-p", "fsc-cli"], timeout=900)
    code, out = run([str(ROOT / "target/debug/fsc"), "fetch-models", "--urls"])
    if code != 0:
        report("FAIL", "models", f"couldn't list them: {out.strip()}")
        return
    for line in out.strip().splitlines():
        label, url = line.split("\t")
        code, head = run(["curl", "-sIL", "--max-time", "30", "-A", UA, url], timeout=40)
        statuses = re.findall(r"^HTTP/\S+ (\d+)", head, re.M)
        etags = re.findall(r"^(?:x-linked-etag|etag):\s*(\S+)", head, re.M | re.I)
        if not statuses or statuses[-1] != "200":
            report("FAIL", "models", f"{label}: {url} gives HTTP {statuses[-1] if statuses else 'nothing'}")
            continue
        tag = etags[0].strip('"W/') if etags else None
        now["models"][url] = tag
        was = tested.get("models", {}).get(url)
        if was and tag and was != tag:
            report("WARN", "models", f"{label}: the file changed upstream; check speech still works "
                   "(delete it from ~/.local/share/fsc/models and `fsc fetch-models`)")
        else:
            report("OK", "models", label)


def check_voices():
    """Every voice the Voice menu offers (crates/fsc-core/src/voices.rs) is
    still in Piper's catalogue, with one speaker."""
    section("Piper voices (Voice menu)")
    keys = re.findall(r'key: "([^"]+)"', (ROOT / "crates/fsc-core/src/voices.rs").read_text())
    try:
        catalogue = json.loads(get("https://huggingface.co/rhasspy/piper-voices/resolve/main/voices.json", timeout=60))
    except Exception as e:
        report("WARN", "voices", f"couldn't fetch voices.json: {e}")
        return
    gone = [k for k in keys if k not in catalogue]
    multi = [k for k in keys if k in catalogue and catalogue[k].get("num_speakers", 1) != 1]
    for k in gone:
        report("FAIL", "voices", f"{k} is no longer in voices.json; drop it from voices.rs (and lang.rs if it's a default)")
    for k in multi:
        report("WARN", "voices", f"{k} now has several speakers")
    if not gone and not multi:
        report("OK", "voices", f"all {len(keys)} voices listed")


def check_actions():
    section("GitHub Actions")
    uses = set()
    for wf in WORKFLOWS.glob("*.y*ml"):
        uses.update(re.findall(r"uses:\s*([\w.-]+/[\w.-]+)@v(\d+)", wf.read_text()))
    for repo, major in sorted(uses):
        code, out = run(["gh", "api", f"repos/{repo}/releases/latest", "--jq", ".tag_name"], timeout=30)
        latest = vkey(out.strip())[:1] if code == 0 else None
        if not latest:
            report("WARN", "actions", f"{repo}@v{major}: couldn't look up the newest ({out.strip()[:80]})")
        elif latest[0] > int(major):
            report("WARN", "actions", f"{repo}@v{major}: v{latest[0]} is out")
        else:
            report("OK", "actions", f"{repo}@v{major}")


def check_build():
    section("Build")
    code, out = run(["cargo", "test", "-q", "--workspace"], timeout=1800)
    report("OK" if code == 0 else "FAIL", "build", "cargo test" + ("" if code == 0 else ":\n" + out[-2000:]))
    code, out = run(["cargo", "clippy", "-q", "--all-targets"], timeout=1800)
    clean = code == 0 and "warning" not in out
    report("OK" if clean else "WARN", "build", "cargo clippy" + ("" if clean else ":\n" + out[-2000:]))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--live", action="store_true", help="rerun every agent's real tests, not just changed ones")
    ap.add_argument("--record", action="store_true", help="after a run with no FAILs, save the versions as tested")
    args = ap.parse_args()

    tested = json.loads(TESTED.read_text()) if TESTED.exists() else {}
    now = {"date": str(date.today()), "agents": {}, "models": {}}
    t0 = time.time()
    check_build()
    check_agents(tested, args.live, now)
    check_packages()
    check_crates()
    check_models(tested, now)
    check_voices()
    check_actions()

    fails = [r for r in results if r[0] == "FAIL"]
    warns = [r for r in results if r[0] == "WARN"]
    print(f"\n{len(fails)} failed, {len(warns)} warnings ({time.time() - t0:.0f}s)")
    if args.record:
        if fails:
            print("not recording: fix the failures first")
        else:
            # Keep entries for agents that weren't installed this time.
            for k in ("agents", "models"):
                now[k] = {**tested.get(k, {}), **now[k]}
            TESTED.write_text(json.dumps(now, indent=2) + "\n")
            print(f"recorded in {TESTED.relative_to(ROOT)}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
