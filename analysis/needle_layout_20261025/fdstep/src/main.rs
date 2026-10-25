//! fdstep: the Crank-Nicolson time loop of fd2d.simulate, in Rust.
//!
//! fd2d.py assembles the exact operators (A = C/dt - L/2 in CSC, B = C/dt + L/2
//! in CSR, the heater source and the sensor weights) and writes one binary case
//! file per slice; this binary factors A once (faer sparse LU), steps
//!     T_{n+1} = A^{-1} (B T_n + s [n dt < dur])
//! and records the weighted sensor readings at the requested steps.
//! Usage: fdstep CASE... ; writes CASE.out (nsens x nout f64, little endian).
//! Threads: RAYON_NUM_THREADS (one case per thread; faer runs sequentially).

use faer::linalg::solvers::Solve;
use faer::sparse::{SparseColMat, SymbolicSparseColMat};
use faer::{Mat, Par};
use rayon::prelude::*;
use std::fs;
use std::io::Write;

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Rd<'a> {
    fn u(&mut self) -> usize {
        let v = u64::from_le_bytes(self.b[self.p..self.p + 8].try_into().unwrap());
        self.p += 8;
        v as usize
    }
    fn f(&mut self) -> f64 {
        let v = f64::from_le_bytes(self.b[self.p..self.p + 8].try_into().unwrap());
        self.p += 8;
        v
    }
    fn us(&mut self, n: usize) -> Vec<usize> {
        (0..n).map(|_| self.u()).collect()
    }
    fn fs(&mut self, n: usize) -> Vec<f64> {
        (0..n).map(|_| self.f()).collect()
    }
}

fn run(path: &str) -> Result<(), String> {
    let buf = fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let mut r = Rd { b: &buf, p: 0 };
    let n = r.u();
    let nnz_a = r.u();
    let nnz_b = r.u();
    let nsteps = r.u();
    let nsens = r.u();
    let nout = r.u();
    let dt = r.f();
    let dur = r.f();
    let a_ptr = r.us(n + 1);
    let a_idx = r.us(nnz_a);
    let a_val = r.fs(nnz_a);
    let b_ptr = r.us(n + 1);
    let b_idx = r.us(nnz_b);
    let b_val = r.fs(nnz_b);
    let src = r.fs(n);
    let mut sens = Vec::with_capacity(nsens);
    for _ in 0..nsens {
        let m = r.u();
        let idx = r.us(m);
        let w = r.fs(m);
        sens.push((idx, w));
    }
    let outsteps = r.us(nout);

    let sym = SymbolicSparseColMat::<usize>::new_checked(n, n, a_ptr, None, a_idx);
    let a = SparseColMat::<usize, f64>::new(sym, a_val);
    let lu = a.sp_lu().map_err(|e| format!("{path}: LU failed {e:?}"))?;

    let mut t = Mat::<f64>::zeros(n, 1);
    let mut rhs = Mat::<f64>::zeros(n, 1);
    let mut rec = vec![0.0f64; nsens * nout];
    let mut ko = 0usize;
    for step in 0..nsteps {
        let heat = (step as f64) * dt < dur - 1e-9;
        for i in 0..n {
            let mut s = 0.0;
            for k in b_ptr[i]..b_ptr[i + 1] {
                s += b_val[k] * t[(b_idx[k], 0)];
            }
            if heat {
                s += src[i];
            }
            rhs[(i, 0)] = s;
        }
        lu.solve_in_place(rhs.as_mut());
        std::mem::swap(&mut t, &mut rhs);
        if ko < nout && outsteps[ko] == step + 1 {
            for (j, (idx, w)) in sens.iter().enumerate() {
                let mut s = 0.0;
                for (q, &i) in idx.iter().enumerate() {
                    s += w[q] * t[(i, 0)];
                }
                rec[j * nout + ko] = s;
            }
            ko += 1;
        }
    }
    let mut out = fs::File::create(format!("{path}.out")).map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = rec.iter().flat_map(|v| v.to_le_bytes()).collect();
    out.write_all(&bytes).map_err(|e| e.to_string())?;
    Ok(())
}

fn main() {
    faer::set_global_parallelism(Par::Seq);
    let files: Vec<String> = std::env::args().skip(1).collect();
    let errs: Vec<String> = files.par_iter().filter_map(|f| run(f).err()).collect();
    for e in &errs {
        eprintln!("{e}");
    }
    if !errs.is_empty() {
        std::process::exit(1);
    }
}
