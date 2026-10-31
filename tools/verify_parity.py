#!/usr/bin/env python3
"""Compare a world this tool wrote with vanilla 26.3's chunk generator.

Vanilla side: Horizon's VanillaChunkGenerationOracle, driven with the same
decoration order (HORIZON_ORDER) this tool wrote. Brahma side: the
`dump` command over the saved region files.

usage:
  python tools/verify_parity.py --horizon /path/to/horizon
      [--resources DIR] [--bin PATH] [--world DIR] [--order FILE]
      [--seed N] [--rect x0,z0,x1,z1] [--radius BLOCKS] [--threads N]
      [--dimension overworld] [--dimension the_nether] [--dimension the_end]

`--dimension` is repeatable. The default is the overworld. Each dimension is
generated into the same world (later ones resume) with its own order file,
then compared on its own. Pass the three vanilla dimensions to cover
overworld, nether and end.
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]


def cargo_bin() -> Path:
    target = HERE / "target" / "release" / ("brahma.exe" if os.name == "nt" else "brahma")
    if not target.exists():
        target = HERE / "target" / "debug" / ("brahma.exe" if os.name == "nt" else "brahma")
    return target


def build_tool() -> Path:
    subprocess.run(["cargo", "build", "--release", "-q"], cwd=HERE, check=True)
    path = cargo_bin()
    if not path.exists():
        raise SystemExit(f"brahma binary missing at {path}")
    return path


def dimension_path(name: str) -> str:
    return name.split(":", 1)[1] if ":" in name else name


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--horizon", required=True, type=Path, help="Horizon checkout (jars, tools, .local)")
    parser.add_argument("--resources", type=Path, help="prepared client resources (default: <horizon>/.local/horizon-client/<version>)")
    parser.add_argument("--bin", type=Path, help="brahma binary (default: cargo build --release)")
    parser.add_argument("--world", type=Path, help="existing world folder to dump (skip generate)")
    parser.add_argument("--order", type=Path, help="existing HORIZON_ORDER file (required with --world; one dimension only)")
    parser.add_argument("--seed", type=int, default=12345)
    parser.add_argument("--rect", action="append", help="chunk rectangle x0,z0,x1,z1 (repeatable)")
    parser.add_argument("--radius", type=int, default=16, help="generate radius in blocks when --world is omitted")
    parser.add_argument("--threads", type=int, default=max(2, (os.cpu_count() or 4) // 2))
    parser.add_argument(
        "--dimension",
        action="append",
        help="overworld, the_nether, the_end, or a namespaced id (repeatable; default overworld)",
    )
    parser.add_argument("--keep", action="store_true", help="keep the generated world and dump files")
    args = parser.parse_args()

    horizon = args.horizon.resolve()
    sys.path.insert(0, str(horizon / "tools"))
    import lab
    import test_chunk_generation_parity as parity

    resources = args.resources or lab.RESOURCES
    if not (resources / "facts.json").exists():
        raise SystemExit(f"prepared resources missing: {resources / 'facts.json'}")
    if not (resources / "worldgen").exists():
        raise SystemExit(f"worldgen bundle missing: {resources / 'worldgen'}")

    dimensions = args.dimension or ["overworld"]
    rects = args.rect or ["-1,-1,1,1"]
    binary = args.bin or build_tool()
    scratch = Path(tempfile.mkdtemp(prefix="brahma-parity-"))
    try:
        if args.world is not None:
            if args.order is None:
                raise SystemExit("--order is required when --world is set")
            if len(dimensions) != 1:
                raise SystemExit("--world takes a single --dimension")
            compare_one(
                parity,
                binary,
                resources,
                args.world,
                args.order,
                dimensions[0],
                args.seed,
                rects,
                scratch,
            )
            return

        world = scratch / "world"
        for dimension in dimensions:
            order = scratch / f"order-{dimension_path(dimension)}.txt"
            print(
                f"generating seed {args.seed} radius {args.radius} {dimension} -> {world}",
                flush=True,
            )
            subprocess.run(
                [
                    str(binary),
                    "generate",
                    "--resources",
                    str(resources),
                    "--out",
                    str(world),
                    "--seed",
                    str(args.seed),
                    "--radius",
                    str(args.radius),
                    "--center",
                    "0",
                    "0",
                    "--threads",
                    str(args.threads),
                    "--dimension",
                    dimension,
                    "--order-file",
                    str(order),
                ],
                check=True,
            )
            compare_one(
                parity,
                binary,
                resources,
                world,
                order,
                dimension,
                args.seed,
                rects,
                scratch,
            )
    finally:
        if not args.keep:
            shutil.rmtree(scratch, ignore_errors=True)
        else:
            print(f"kept {scratch}")


def compare_one(parity, binary, resources, world, order, dimension, seed, rects, scratch) -> None:
    path = dimension_path(dimension)
    os.environ["HORIZON_ORDER"] = str(order)
    os.environ["HORIZON_PHASE"] = "full"
    print(f"running vanilla oracle ({path})", flush=True)
    vanilla_path = scratch / f"vanilla-{path}.txt"
    rust_path = scratch / f"rust-{path}.txt"
    oracle = parity.Oracle()
    oracle.run(vanilla_path, ["dump", seed, path, *rects])

    print(f"dumping saved world ({path})", flush=True)
    with open(rust_path, "w", encoding="utf-8") as handle:
        subprocess.run(
            [
                str(binary),
                "dump",
                "--resources",
                str(resources),
                "--dimension",
                path,
                str(world),
                *rects,
            ],
            stdout=handle,
            check=True,
        )

    vanilla, rust = parity.parse(vanilla_path), parity.parse(rust_path)
    names = parity.state_names()
    totals = [0, 0, 0, 0]
    for rect in rects:
        x0, z0, x1, z1 = map(int, rect.split(","))
        part = {k: v for k, v in vanilla.items() if x0 <= k[0] <= x1 and z0 <= k[1] <= z1}
        one = parity.compare(part, rust, names, f"seed {seed} {path} {rect}")
        totals = [a + b for a, b in zip(totals, one)]
    print(
        f"{path}: compared {len(vanilla)} chunks: differing chunks {totals[0]} "
        f"(blocks {totals[1]}, biome cells {totals[2]}, block entities {totals[3]})"
    )
    if totals[0]:
        sys.exit(1)


if __name__ == "__main__":
    main()
