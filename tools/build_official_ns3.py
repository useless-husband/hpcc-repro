#!/usr/bin/env python3
"""Builds the authors' ns-3.17-based HPCC simulator without waf, for the
cross-check described in the README. Its waf 1.7 needs Python 2, which is not
available, so this script compiles the ten ns-3 modules that scratch/third.cc
needs directly with clang++ (4 parallel jobs) and links `third`.

    git clone https://github.com/alibaba-edu/High-Precision-Congestion-Control OFFICIAL
    python3 tools/build_official_ns3.py OFFICIAL/simulation BUILD_DIR

Nothing of the authors' code is copied into this repository.
"""
import glob
import os
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

if len(sys.argv) != 3:
    sys.exit(__doc__)
SIM = os.path.abspath(sys.argv[1])
OUT = os.path.abspath(sys.argv[2])
MODS = ["core", "network", "bridge", "mpi", "internet", "point-to-point", "config-store", "stats", "tools", "applications"]
inc = os.path.join(OUT, "include", "ns3")
os.makedirs(inc, exist_ok=True)
objdir = os.path.join(OUT, "obj"); os.makedirs(objdir, exist_ok=True)

def lists(ws, var):
    # first assignment "X.source = [ ... ]" for var in (module, headers)
    m = re.search(var + r"\.source\s*=\s*\[(.*?)\]", ws, re.S)
    return re.findall(r"'([^']+)'|\"([^\"]+)\"", m.group(1)) if m else []

srcs = []
for mod in MODS:
    ws = open(os.path.join(SIM, "src", mod, "wscript")).read()
    mvar = re.search(r"(\w+)\s*=\s*bld\.create_ns3_module\(", ws).group(1)
    hvar = re.search(r"(\w+)\s*=\s*bld\(features=\['ns3header'\]\)", ws)
    hvar = hvar.group(1) if hvar else "headers"
    s = [a or b for a, b in lists(ws, mvar)]
    h = [a or b for a, b in lists(ws, hvar)]
    listed = set(os.path.basename(x) for x in h)
    h = h + [os.path.relpath(q, os.path.join(SIM, "src", mod)) for q in glob.glob(os.path.join(SIM, "src", mod, "*", "*.h"))
             if os.path.basename(q) not in listed]
    if mod == "core":
        s += [x for x in ["model/int64x64-128.cc", "model/unix-system-thread.cc", "model/unix-system-mutex.cc",
                          "model/unix-system-condition.cc", "model/unix-fd-reader.cc", "model/system-thread.cc",
                          "model/unix-system-wall-clock-ms.cc", "model/realtime-simulator-impl.cc",
                          "model/wall-clock-synchronizer.cc", "model/system-condition.cc"]
              if os.path.exists(os.path.join(SIM, "src", mod, x)) and x not in s]
    hs = []
    for x in h:
        p = os.path.join(SIM, "src", mod, x)
        d = os.path.join(inc, os.path.basename(x))
        if os.path.exists(p) and not os.path.exists(d):
            os.symlink(p, d)
        if os.path.basename(x) in listed and os.path.basename(x) not in hs:
            hs.append(os.path.basename(x))
    with open(os.path.join(inc, mod + "-module.h"), "w") as f:
        f.write("".join(f'#include "ns3/{x}"\n' for x in hs))
    srcs += [os.path.join(SIM, "src", mod, x) for x in s if os.path.exists(os.path.join(SIM, "src", mod, x))]
    print(mod, len(s), "sources", len(hs), "headers")
with open(os.path.join(inc, "core-config.h"), "w") as f:
    f.write("#ifndef CORE_CONFIG_H\n#define CORE_CONFIG_H\n" + "".join(f"#define {d} 1\n" for d in [
        "HAVE___UINT128_T", "HAVE_STDINT_H", "HAVE_INTTYPES_H", "HAVE_SYS_TYPES_H", "HAVE_SYS_STAT_H",
        "HAVE_DIRENT_H", "HAVE_STDLIB_H", "HAVE_GETENV", "HAVE_SIGNAL_H", "HAVE_PTHREAD_H", "INT64X64_USE_128"]) + "#endif\n")
for extra in ["config-store-config.h"]:
    open(os.path.join(inc, extra), "w").write("")
CXX = ["clang++", "-std=c++11", "-O2", "-w", "-fPIC", "-DNS3_LOG_ENABLE", "-DNS3_ASSERT_ENABLE", "-I" + os.path.join(OUT, "include")]
def cc(src):
    obj = os.path.join(objdir, src.replace(SIM + "/", "").replace("/", "_") + ".o")
    if os.path.exists(obj): return None
    p = subprocess.run(CXX + ["-c", src, "-o", obj], capture_output=True, text=True)
    return None if p.returncode == 0 else (src, "\n".join(l for l in p.stderr.splitlines() if "error" in l)[:1500])
srcs.append(os.path.join(SIM, "scratch", "third.cc"))
with ThreadPoolExecutor(4) as ex:
    errs = [e for e in ex.map(cc, srcs) if e]
print("compile errors:", len(errs))
for s, e in errs[:8]:
    print("---", s); print(e)
if not errs:
    objs = [os.path.join(objdir, f) for f in os.listdir(objdir)]
    p = subprocess.run(["clang++", "-o", os.path.join(OUT, "third")] + objs, capture_output=True, text=True)
    print("link:", p.returncode, p.stderr[-1500:])
