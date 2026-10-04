#!/usr/bin/env python3
"""Mutation check: each mutant is a deliberate one-line bug; the test suite
must fail for every one of them.

    python3 tools/mutants.py [--write docs/mutants.md] [--only NAME]

The repository is copied to a temporary directory first, so the working
tree (and any experiment using target/release) is never touched.
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# (name, file, original line fragment, mutated fragment, what it breaks)
MUTANTS = [
    ("hpcc_wrong_hop_qlen", "src/cc/hpcc.rs",
     "let qlen = cur.qlen.min(last.qlen) as f64;",
     "let qlen = hops[0].qlen.min(self.l[0].qlen) as f64;",
     "HPCC reads the queue length of the first hop instead of hop i"),
    ("hpcc_wc_every_ack", "src/cc/hpcc.rs",
     "self.w = self.compute_wind(u, false);",
     "self.w = self.compute_wind(u, true);",
     "HPCC updates the reference window W^c on every ACK (overreaction)"),
    ("hpcc_ignores_eta", "src/cc/hpcc.rs",
     "w = self.clamp_w(self.wc / (u / eta) + self.p.w_ai);",
     "w = self.clamp_w(self.wc / u + self.p.w_ai);",
     "HPCC targets 100% utilisation instead of eta"),
    ("hpcc_tau_not_capped", "src/cc/hpcc.rs",
     "let tau = (tau_ps as f64 * 1e-12).min(self.t);",
     "let tau = tau_ps as f64 * 1e-12;",
     "EWMA weight tau/T may exceed 1 (Algorithm 1 line 8 dropped)"),
    ("pfc_pause_off_by_one", "src/mmu.rs",
     "(self.hdrm[port] > 0 || self.shared_used_by(port) >= self.threshold(port))",
     "(self.hdrm[port] > 0 || self.shared_used_by(port) > self.threshold(port))",
     "PFC pause triggers one byte late"),
    ("pfc_resume_off_by_one", "src/mmu.rs",
     "used + self.cfg.resume_offset <= self.threshold(port)",
     "used + self.cfg.resume_offset < self.threshold(port)",
     "PFC resume hysteresis off by one byte"),
    ("mmu_shared_leak", "src/mmu.rs",
     "self.shared_used -= from_shared;",
     "self.shared_used -= from_ingress;",
     "shared-buffer accounting releases reserved bytes too"),
    ("ecn_k_wrong_scale", "src/sim.rs",
     "let k = (30_000u128 * r / 10_000_000_000) as u64;",
     "let k = (30_000u128 * r / 100_000_000_000) as u64;",
     "DCTCP marking threshold not scaled with link speed"),
    ("serialization_units", "src/time.rs",
     "let num = 8 * SEC;",
     "let num = SEC;",
     "per-byte serialization time computed in bytes instead of bits"),
    ("event_ties_lifo", "src/event.rs",
     "o.time.cmp(&self.time).then_with(|| o.seq.cmp(&self.seq))",
     "o.time.cmp(&self.time).then_with(|| self.seq.cmp(&o.seq))",
     "simultaneous events run in reverse order"),
    ("ecmp_ignores_dport", "src/routing.rs",
     "let b = ((sport as u64) << 32) | dport as u64;",
     "let b = (sport as u64) << 32;",
     "ECMP hash ignores the destination port"),
    ("dcqcn_alpha_sign", "src/cc/dcqcn.rs",
     "self.alpha = (1.0 - self.p.g) * self.alpha + self.p.g;",
     "self.alpha = (1.0 - self.p.g) * self.alpha - self.p.g;",
     "DCQCN alpha decreases when CNPs arrive"),
    ("drops_not_counted", "src/sim.rs",
     "self.counters.dropped_bytes += size;",
     "self.counters.dropped_bytes += 0;",
     "switch drops vanish from the byte accounting"),
    ("no_pacing", "src/sim.rs",
     "f.next_avail = now + ser_time(size as u64, f.ctl.rate_bps);",
     "f.next_avail = now;",
     "NIC ignores the CC's pacing rate"),
    ("ideal_no_ack_serialization", "src/sim.rs",
     "let end = start + ser_time(ack as u64, rate);",
     "let end = start;",
     "ideal FCT forgets to serialize the ACK on the way back"),
    ("acks_never_share_data_queue", "src/sim.rs",
     "Kind::Ack | Kind::Nack if self.cfg.ack_class == AckClass::Control => {",
     "Kind::Ack | Kind::Nack if true => {",
     "switches always prioritise ACKs, ignoring the configured ACK class"),
    ("duplicates_not_reacked", "src/sim.rs",
     """            // Duplicate (a go-back-N retransmission of data already
            // received): re-acknowledge, otherwise a lost final ACK would make
            // the sender retransmit forever.
            Some(Kind::Ack)""",
     """            None""",
     "receiver ignores duplicate data instead of re-acknowledging it"),
    ("late_ack_leaves_snd_nxt_behind", "src/sim.rs",
     """            if f.snd_nxt < f.snd_una {
                f.snd_nxt = f.snd_una;
            }
        }
        let view""",
     """        }
        let view""",
     "a late ACK after a go-back-N rewind leaves snd_nxt behind snd_una"),
    ("int_qlen_before_dequeue", "src/sim.rs",
     "qlen: port.data_bytes + port.ctrl_bytes,",
     "qlen: 0,",
     "switches report an empty queue in INT"),
]


def copy_repo(dst):
    for item in ["Cargo.toml", "Cargo.lock", "src", "tests"]:
        s = os.path.join(ROOT, item)
        if os.path.isdir(s):
            shutil.copytree(s, os.path.join(dst, item))
        elif os.path.exists(s):
            shutil.copy2(s, os.path.join(dst, item))


def run_tests(work, target):
    env = dict(os.environ, CARGO_TARGET_DIR=target)
    try:
        p = subprocess.run(["cargo", "test", "--release", "-j", "4", "--no-fail-fast"], cwd=work, env=env,
                           capture_output=True, text=True, timeout=900)
    except subprocess.TimeoutExpired:
        # A mutant that makes the suite hang is detected, but say so.
        return 1, ["(suite timed out after 900 s)"], False
    out = p.stdout + p.stderr
    failed = sorted(set(re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M)))
    compile_error = "error[E" in out or "error: could not compile" in out
    return p.returncode, failed, compile_error


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--write", default=None, help="write a markdown table to this file")
    ap.add_argument("--only", default=None)
    a = ap.parse_args()
    muts = [m for m in MUTANTS if not a.only or a.only in m[0]]
    tmp = tempfile.mkdtemp(prefix="hpcc-mutants-")
    target = os.path.join(tmp, "target")
    base = os.path.join(tmp, "base")
    os.makedirs(base)
    copy_repo(base)
    rc, failed, _ = run_tests(base, target)
    if rc != 0:
        sys.exit(f"baseline test suite fails, fix that first: {failed}")
    rows = []
    survivors = 0
    for name, path, old, new, what in muts:
        work = os.path.join(tmp, name)
        shutil.copytree(base, work)
        f = os.path.join(work, path)
        src = open(f).read()
        if src.count(old) != 1:
            sys.exit(f"mutant {name}: fragment found {src.count(old)} times in {path}")
        open(f, "w").write(src.replace(old, new))
        t0 = time.time()
        rc, failed, cerr = run_tests(work, target)
        dt = time.time() - t0
        if cerr:
            verdict, detail = "compile error", "-"
        elif rc != 0:
            verdict, detail = "killed", ", ".join(failed) if failed else "(test binary aborted)"
        else:
            verdict, detail = "SURVIVED", "-"
            survivors += 1
        rows.append((name, path, what, verdict, detail))
        print(f"{name:<26} {verdict:<9} {dt:5.1f}s  {detail}", flush=True)
        shutil.rmtree(work)
    shutil.rmtree(tmp)
    if a.write:
        with open(a.write, "w") as out:
            out.write("# Mutation check\n\n")
            out.write("Each mutant is a deliberate one-line bug applied to a copy of the repository; the full test\n")
            out.write("suite (`cargo test --release`) must fail. Regenerate with `make mutants`.\n\n")
            out.write("| Mutant | File | Bug | Result | Failing tests |\n|---|---|---|---|---|\n")
            for name, path, what, verdict, detail in rows:
                out.write(f"| `{name}` | `{path}` | {what} | {verdict} | {detail} |\n")
            only_golden = [r[0] for r in rows if r[3] == "killed" and r[4] == "golden_digest"]
            out.write(f"\n{len(rows) - survivors} of {len(rows)} mutants killed. `golden_digest` fails on any change of "
                      "simulated behaviour, so it is not counted as a targeted kill; ")
            if only_golden:
                out.write("mutants killed only by it: " + ", ".join(f"`{m}`" for m in only_golden) + ".\n")
            else:
                out.write("every mutant is also killed by at least one targeted test.\n")
    print(f"{len(rows) - survivors}/{len(rows)} killed")
    sys.exit(1 if survivors else 0)


if __name__ == "__main__":
    main()
