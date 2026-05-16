#!/usr/bin/env python3
"""Tiny helper for the Reich demo molecular-sex call.

Takes nX and nY on argv and emits the lab's sex call. Implements the
lab protocol exactly: female < 0.03, male > 0.32, low_coverage if
nX+nY < 200, with ambiguity bucket XX? / XY? around 0.15.
"""
import sys


def call(nx: int, ny: int) -> tuple[str, str]:
    total = nx + ny
    if total < 200:
        return ("NA", "low_coverage")
    ry = ny / total
    if ry < 0.03:
        return (f"{ry:.4f}", "XX")
    if ry > 0.32:
        return (f"{ry:.4f}", "XY")
    if ry < 0.15:
        return (f"{ry:.4f}", "XX?")
    return (f"{ry:.4f}", "XY?")


if __name__ == "__main__":
    nx, ny = int(sys.argv[1]), int(sys.argv[2])
    ry, label = call(nx, ny)
    print(f"{ry}\t{label}")
