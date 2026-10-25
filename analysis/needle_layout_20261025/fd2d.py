"""Generalised Darcy + Crank-Nicolson heat-pulse slice (the needle_cost.py physics).

`../flow_vector_research_20261024/needle_cost.py` solves one fixed set of
layouts with one needle radius.  This module keeps its discretisation and
material model unchanged and only opens up the inputs:

  * any list of needle centres, each with its own steel radius and its own
    wound (impermeable, isotropic kT-conducting) radius;
  * any grid spacing and domain (needle_cost: dx = 0.02 cm, heat
    x in [-2, 3], y in [-2, 2], Darcy x in [-12, 13]);
  * per-slice wood properties (kL, anisotropy R, rho c) and heater energy.

Physics (unchanged from needle_cost.py, README sec. 6 there):
  Darcy q = -kappa grad p, div q = 0, kappa = diag(Kp, 1) in the grain frame
  (grain and flow along +x), impermeable needle + wound cells kappa = 1e-9,
  p = -(U/Kp) x on the left/right faces, no flux top/bottom; harmonic face
  permeabilities, 5-point finite volume.  The discretely divergence-free face
  fluxes drive a conservative central advection term; conduction uses
  harmonic face conductivities; Crank-Nicolson, dt = 0.1 s, T = 0 outside.
  The sensor reading is the mean temperature over the sensor's steel cells.

`check()` reproduces needle_cost.simulate() for std_axial (to round-off).
"""

from __future__ import annotations

import sys
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
import scipy.sparse as sp
from scipy.sparse.linalg import splu, spsolve

HERE = Path(__file__).resolve().parent
FV = HERE.parent / "flow_vector_research_20261024"
sys.path.insert(0, str(FV))
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent / "compression_research_20261024"))

import needle_cost as nc  # noqa: E402
import vecmodel as vm  # noqa: E402

C_WOOD = nc.C_WOOD  # J/cm^3/K
KL = nc.KL
R_ANISO = nc.R_ANISO
C_NEEDLE = nc.C_NEEDLE
LAM_NEEDLE = nc.LAM_NEEDLE
DT = nc.DT
T_END = nc.T_END
T_OUT = nc.T_OUT
DUR = vm.DUR

E_NEW = 4.8  # J/cm, heater energy for the flex-needle design (user spec)
A_NEW = E_NEW / (4 * np.pi * C_WOOD)  # degC cm^2, analytic amplitude for E_NEW in wood rho c = 2.8


@dataclass
class Grid:
    dx: float = nc.DX
    hx: tuple = (nc.HX0, nc.HX1)
    y: tuple = (nc.Y0, nc.Y1)
    darcy_x: tuple = (nc.DX0, nc.DX1)
    darcy_y: tuple | None = None  # None: same as heat y

    def heat_axes(self):
        xs = np.arange(self.hx[0] + self.dx / 2, self.hx[1] - 1e-9, self.dx)
        ys = np.arange(self.y[0] + self.dx / 2, self.y[1] - 1e-9, self.dx)
        return xs, ys

    def darcy_axes(self):
        yy = self.y if self.darcy_y is None else self.darcy_y
        xs = np.arange(self.darcy_x[0] + self.dx / 2, self.darcy_x[1] - 1e-9, self.dx)
        ys = np.arange(yy[0] + self.dx / 2, yy[1] - 1e-9, self.dx)
        return xs, ys


@dataclass
class Needle:
    x: float
    y: float
    r: float  # steel radius, cm
    w: float  # wound radius (impermeable + kT conducting), cm; <= r means no wound


@dataclass
class Wood:
    kL: float = KL
    R: float = R_ANISO
    C: float = C_WOOD


def disk_fraction(xs, ys, cx, cy, r, dx, aa):
    """Area fraction of each cell inside the disk.  aa = 0: needle_cost's
    binary mask (cell centre inside); aa = n: n x n sub-samples per cell, so
    the geometry moves continuously with sub-cell needle displacements."""
    if aa == 0:
        X, Y = np.meshgrid(xs, ys, indexing="ij")
        return (np.hypot(X - cx, Y - cy) <= r).astype(float)
    f = np.zeros((len(xs), len(ys)))
    i = np.nonzero(np.abs(xs - cx) <= r + dx)[0]
    j = np.nonzero(np.abs(ys - cy) <= r + dx)[0]
    if len(i) == 0 or len(j) == 0:
        return f
    o = (np.arange(aa) + 0.5) / aa - 0.5
    X = xs[i][:, None, None, None] + dx * o[None, None, :, None]
    Y = ys[j][None, :, None, None] + dx * o[None, None, None, :]
    f[np.ix_(i, j)] = (np.hypot(X - cx, Y - cy) <= r).mean(axis=(2, 3))
    return f


def darcy(needles, Kp, U, grid: Grid, aa=0):
    """needle_cost.darcy with a per-needle blocked radius.  qx (nx+1, ny), qy (nx, ny+1) in cm/h.

    aa > 0: partially blocked cells get permeability (1 - f) kappa + f 1e-9."""
    xs, ys = grid.darcy_axes()
    dx = grid.dx
    X0, X1 = grid.darcy_x
    nx, ny = len(xs), len(ys)
    kx = np.full((nx, ny), float(Kp))
    ky = np.ones((nx, ny))
    for n in needles:
        f = disk_fraction(xs, ys, n.x, n.y, max(n.w, n.r), dx, aa)
        kx = kx * (1 - f) + 1e-9 * f
        ky = ky * (1 - f) + 1e-9 * f
    idx = np.arange(nx * ny).reshape(nx, ny)
    gx = 2 * kx[:-1] * kx[1:] / (kx[:-1] + kx[1:])
    gy = 2 * ky[:, :-1] * ky[:, 1:] / (ky[:, :-1] + ky[:, 1:])
    rows, cols, vals = [], [], []
    diag = np.zeros(nx * ny)
    rhs = np.zeros(nx * ny)
    for a, c_, g in ((idx[:-1].ravel(), idx[1:].ravel(), gx.ravel()), (idx[:, :-1].ravel(), idx[:, 1:].ravel(), gy.ravel())):
        rows += [a, c_]; cols += [c_, a]; vals += [g, g]
        np.add.at(diag, a, -g); np.add.at(diag, c_, -g)
    G = U / Kp
    for i, xf in ((0, X0), (nx - 1, X1)):
        g = 2 * kx[i]
        diag[idx[i]] -= g
        rhs[idx[i]] -= g * (-G * xf)
    A = sp.csr_matrix((np.concatenate(vals), (np.concatenate(rows), np.concatenate(cols))), shape=(nx * ny,) * 2) + sp.diags(diag)
    p = spsolve(A.tocsc(), rhs).reshape(nx, ny)
    qx = np.zeros((nx + 1, ny))
    qx[1:-1] = -gx * (p[1:] - p[:-1]) / dx
    qx[0] = -2 * kx[0] * (p[0] - (-G * X0)) / dx
    qx[-1] = -2 * kx[-1] * ((-G * X1) - p[-1]) / dx
    qy = np.zeros((nx, ny + 1))
    qy[:, 1:-1] = -gy * (p[:, 1:] - p[:, :-1]) / dx
    return xs, ys, qx, qy


def heat_flux(needles, Kp, U, grid: Grid, aa=0):
    """Darcy face fluxes cropped to the heat grid (cm/h)."""
    xh, yh = grid.heat_axes()
    if U == 0.0:
        return np.zeros((len(xh) + 1, len(yh))), np.zeros((len(xh), len(yh) + 1))
    xs, ys, qx, qy = darcy(needles, Kp, U, grid, aa)
    i0 = int(round((grid.hx[0] - grid.darcy_x[0]) / grid.dx))
    j0 = int(round((grid.y[0] - (grid.y if grid.darcy_y is None else grid.darcy_y)[0]) / grid.dx))
    n = len(xh)
    m = len(yh)
    if j0 < 0 or j0 + m > len(ys):  # Darcy y narrower than heat y: pad with the far field
        QX = np.zeros((n + 1, m)); QX[:] = U
        QY = np.zeros((n, m + 1))
        jj0 = -j0
        QX[:, jj0:jj0 + len(ys)] = qx[i0:i0 + n + 1]
        QY[:, jj0:jj0 + len(ys) + 1] = qy[i0:i0 + n]
        QY[:, jj0] = 0.0; QY[:, jj0 + len(ys)] = 0.0
        return QX, QY
    return qx[i0:i0 + n + 1, j0:j0 + m], qy[i0:i0 + n, j0:j0 + m + 1]


def simulate(needles, heater, sensors, U, Kp, wood: Wood = Wood(), grid: Grid = Grid(), E=None,
             kind="steel", wound_lam="kT", return_field=False, aa=0):
    """Curves (len(sensors), len(T_OUT)) in degC for one heater pulse (Python time loop)."""
    ops = assemble(needles, heater, sensors, U, Kp, wood, grid, E, kind, wound_lam, aa)
    lu = splu(ops["A"])
    B, src, Wsp = ops["B"], ops["src"], ops["W"]
    T = np.zeros(B.shape[0])
    rec = np.zeros((len(sensors), len(T_OUT)))
    ko = 0
    for n in range(ops["nsteps"]):
        T = lu.solve(B @ T + (src if n * DT < DUR - 1e-9 else 0.0))
        if ko < len(T_OUT) and ops["outsteps"][ko] == n + 1:
            rec[:, ko] = Wsp @ T
            ko += 1
    if return_field:
        return rec, ops["field"]
    return rec


def assemble(needles, heater, sensors, U, Kp, wood: Wood = Wood(), grid: Grid = Grid(), E=None,
             kind="steel", wound_lam="kT", aa=0):
    """Crank-Nicolson operators for one slice: A = C/dt - L/2 (CSC), B = C/dt + L/2 (CSR),
    the heater source, the sensor reading weights and the output steps.

    kind="steel": needle_cost physics.  kind="wood": validation, needles with
    wood properties, no wound, uniform flow (needle_cost's "woodneedle").
    aa > 0: area-fraction (anti-aliased) needles and wounds; properties are
    area-weighted, the source and the sensor reading are steel-area weighted.
    aa = 0 is needle_cost's binary masks exactly."""
    E = vm.A_REF * 4 * np.pi * C_WOOD if E is None else E
    xs, ys = grid.heat_axes()
    dx = grid.dx
    nx, ny = len(xs), len(ys)
    lamT = wood.kL / wood.R * wood.C
    lamx = np.full((nx, ny), wood.kL * wood.C)
    lamy = np.full((nx, ny), lamT)
    C = np.full((nx, ny), wood.C)
    lw = lamT if wound_lam == "kT" else wound_lam
    masks = []
    for n in needles:
        fs = disk_fraction(xs, ys, n.x, n.y, n.r, dx, aa)
        if kind == "steel":
            if n.w > n.r:
                fw = disk_fraction(xs, ys, n.x, n.y, n.w, dx, aa) - fs
                lamx = lamx * (1 - fw) + lw * fw
                lamy = lamy * (1 - fw) + lw * fw
            lamx = lamx * (1 - fs) + LAM_NEEDLE * fs
            lamy = lamy * (1 - fs) + LAM_NEEDLE * fs
            C = C * (1 - fs) + C_NEEDLE * fs
        masks.append(fs)
    if kind == "steel":
        qx, qy = heat_flux(needles, Kp, U, grid, aa)
    else:
        qx = np.full((nx + 1, ny), float(U))
        qy = np.zeros((nx, ny + 1))
    qx, qy = qx / 3600.0, qy / 3600.0

    N = nx * ny
    idx = np.arange(N).reshape(nx, ny)
    rows, cols, vals = [], [], []
    diag = np.zeros(N)
    gx = 2 * lamx[:-1] * lamx[1:] / (lamx[:-1] + lamx[1:]) / dx**2
    gy = 2 * lamy[:, :-1] * lamy[:, 1:] / (lamy[:, :-1] + lamy[:, 1:]) / dx**2
    for a, c_, g in ((idx[:-1].ravel(), idx[1:].ravel(), gx.ravel()), (idx[:, :-1].ravel(), idx[:, 1:].ravel(), gy.ravel())):
        rows += [a, c_]; cols += [c_, a]; vals += [g, g]
        np.add.at(diag, a, -g); np.add.at(diag, c_, -g)
    for sl, lam in ((np.s_[0, :], lamx), (np.s_[-1, :], lamx), (np.s_[:, 0], lamy), (np.s_[:, -1], lamy)):
        np.add.at(diag, idx[sl].ravel(), -2 * lam[sl].ravel() / dx**2)
    for a, c_, q in ((idx[:-1].ravel(), idx[1:].ravel(), qx[1:-1].ravel()), (idx[:, :-1].ravel(), idx[:, 1:].ravel(), qy[:, 1:-1].ravel())):
        f = wood.C * q / (2 * dx)
        rows += [a, a, c_, c_]; cols += [a, c_, a, c_]; vals += [-f, -f, f, f]
    Lop = sp.csr_matrix((np.concatenate(vals), (np.concatenate(rows), np.concatenate(cols))), shape=(N, N)) + sp.diags(diag)
    Cv = C.ravel()
    A = (sp.diags(Cv / DT) - 0.5 * Lop).tocsc()
    A.sort_indices()
    B = (sp.diags(Cv / DT) + 0.5 * Lop).tocsr()
    B.sort_indices()
    hm = masks[heater].ravel()
    src = E / DUR * hm / (hm.sum() * dx**2)
    W = sp.csr_matrix(np.stack([masks[s].ravel() / masks[s].sum() for s in sensors]))  # reading weights
    nsteps = int(round(T_END / DT))
    outsteps, ko = [], 0
    for n in range(nsteps):
        if ko < len(T_OUT) and abs((n + 1) * DT - T_OUT[ko]) < 1e-6:
            outsteps.append(n + 1)
            ko += 1
    return dict(A=A, B=B, src=src, W=W, nsteps=nsteps, outsteps=np.array(outsteps),
                field=(xs, ys, qx * 3600.0, qy * 3600.0))


# ---------------------------------------------------------------------------
# Rust time loop (fdstep/): same operators, written to a case file per slice

FDSTEP = HERE / "fdstep" / "target" / "release" / "fdstep"


def write_case(path, ops):
    A, B, W = ops["A"], ops["B"], ops["W"]
    n = A.shape[0]
    u = lambda a: np.asarray(a, "<u8").tobytes()  # noqa: E731
    f = lambda a: np.asarray(a, "<f8").tobytes()  # noqa: E731
    parts = [u([n, A.nnz, B.nnz, ops["nsteps"], W.shape[0], len(ops["outsteps"])]), f([DT, DUR]),
             u(A.indptr), u(A.indices), f(A.data), u(B.indptr), u(B.indices), f(B.data), f(ops["src"])]
    for j in range(W.shape[0]):
        row = W.getrow(j)
        parts += [u([row.nnz]), u(row.indices), f(row.data)]
    parts.append(u(ops["outsteps"]))
    Path(path).write_bytes(b"".join(parts))


def simulate_many(specs, workdir, threads=None, assemble_workers=8):
    """Run many slices through the Rust loop.  specs: list of kwargs for assemble().
    Returns a list of (nsens, len(T_OUT)) arrays.  The Python assembly uses at
    most 8 workers; the Rust binary uses every core (rayon default) unless
    `threads` is given."""
    import os
    import subprocess
    from concurrent.futures import ProcessPoolExecutor
    workdir = Path(workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    paths = [workdir / f"case_{i:05d}.bin" for i in range(len(specs))]
    with ProcessPoolExecutor(max_workers=assemble_workers) as ex:
        list(ex.map(_write_one, [(str(p), s) for p, s in zip(paths, specs)]))
    env = dict(os.environ)
    if threads:
        env["RAYON_NUM_THREADS"] = str(threads)
    subprocess.run([str(FDSTEP)] + [str(p) for p in paths], check=True, env=env)
    out = []
    for p, s in zip(paths, specs):
        nsens = len(s["sensors"])
        out.append(np.fromfile(str(p) + ".out", "<f8").reshape(nsens, len(T_OUT)))
        p.unlink()
        Path(str(p) + ".out").unlink()
    return out


def _write_one(args):
    path, spec = args
    write_case(path, assemble(**spec))


def check():
    """Regression: reproduce needle_cost.simulate for std_axial, 2 mm wound, Kp 300, 10 cm/h."""
    ref = nc.simulate("std_axial", 0.0, 10.0, 0.2, 300.0)
    ndl = [Needle(0, 0, nc.R_NEEDLE, 0.1), Needle(0.6, 0, nc.R_NEEDLE, 0.1), Needle(-0.6, 0, nc.R_NEEDLE, 0.1)]
    mine = simulate(ndl, 0, [1, 2], 10.0, 300.0).ravel()
    return float(np.max(np.abs(mine - ref)))


def check_rust(workdir):
    """The Rust loop against the Python loop on one anti-aliased Study-B-like slice."""
    ndl = [Needle(0, 0, 0.045, 0.08), Needle(0.55, 0.03, 0.041, 0.076), Needle(-0.6, 0, 0.041, 0.076)]
    spec = dict(needles=ndl, heater=0, sensors=[1, 2], U=12.0, Kp=300.0, wood=Wood(0.0025, 1.85, 2.8), E=E_NEW, aa=4)
    py = simulate(**spec)
    rs = simulate_many([spec], workdir, threads=1)[0]
    return float(np.max(np.abs(py - rs))), float(np.max(np.abs(py)))


if __name__ == "__main__":
    print("max |fd2d - needle_cost| (degC):", check())
    if FDSTEP.exists():
        print("max |rust - python| (degC), peak:", check_rust(HERE / "results" / "_fdwork"))
