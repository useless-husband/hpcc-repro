//! Guards the argument that simulation results are identical on every
//! platform (see docs/DESIGN.md, "Determinism"): the model may only use
//! exactly-rounded IEEE-754 operations, so any libm transcendental call,
//! fused multiply-add, hash-map iteration or unstable sort in the simulator
//! sources fails this test.

use hpcc_sim::rng::det_ln;
use std::path::Path;

fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push((
                p.display().to_string(),
                std::fs::read_to_string(&p).unwrap(),
            ));
        }
    }
}

#[test]
fn simulator_uses_no_platform_dependent_float_or_iteration_order() {
    let mut files = Vec::new();
    sources(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        &mut files,
    );
    assert!(files.len() > 10);
    let banned = [
        ".ln(",
        ".log(",
        ".log2(",
        ".log10(",
        ".exp(",
        ".exp2(",
        ".powf(",
        ".powi(",
        ".sqrt(",
        ".cbrt(",
        ".sin(",
        ".cos(",
        ".tan(",
        ".hypot(",
        ".mul_add(",
        ".ln_1p(",
        ".exp_m1(",
        "HashMap",
        "HashSet",
        "sort_unstable",
    ];
    for (path, src) in &files {
        // Unit tests may compare against libm; only non-test code matters.
        let code = src.split("#[cfg(test)]").next().unwrap();
        for b in banned {
            assert!(
                !code.contains(b),
                "{path} uses `{b}`, which can differ between platforms"
            );
        }
    }
}

#[test]
fn det_ln_bits_are_pinned() {
    // Bit patterns computed on macOS/arm64. det_ln uses only + - * / and
    // bit manipulation, so they must be the same everywhere.
    let cases: [(f64, u64); 5] = [
        (0.5, PINNED[0]),
        (1e-300, PINNED[1]),
        (0.123456789, PINNED[2]),
        (3.0, PINNED[3]),
        (1e6, PINNED[4]),
    ];
    let got: Vec<u64> = cases.iter().map(|(x, _)| det_ln(*x).to_bits()).collect();
    println!("det_ln bits: {got:?}");
    let want: Vec<u64> = cases.iter().map(|c| c.1).collect();
    assert_eq!(got, want);
}

const PINNED: [u64; 5] = [
    13827790571168217583,
    13872659378474614709,
    13835264914779626866,
    4607626529066517258,
    4623967059079856032,
];
