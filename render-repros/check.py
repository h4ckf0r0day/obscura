#!/usr/bin/env python3
"""Check deterministic repro behavior and report native-coordinate image deltas.

Behavior assertions are based on solid-color component geometry, so font
anti-aliasing does not decide pass/fail. Pair metrics are diagnostics only:
they use the full equal-sized canvas with no registration, cropping, blank-page
exclusion, or opaque aggregate "parity" verdict.
Structural diagnostics include both lossy row/column projections and a
bidirectional nearest-edge distance that preserves two-dimensional placement.
"""

import json
import sys
from pathlib import Path

import numpy as np
from PIL import Image
from scipy.ndimage import (
    distance_transform_edt,
    find_objects,
    gaussian_filter,
    gaussian_filter1d,
    label,
    sobel,
)


def rgb(value):
    value = value.removeprefix("#")
    return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4))


def components(array, color):
    mask = np.all(array == color, axis=2)
    labels, _ = label(mask)
    found = []
    for slices in find_objects(labels):
        if slices is None:
            continue
        height = slices[0].stop - slices[0].start
        width = slices[1].stop - slices[1].start
        area = int(mask[slices].sum())
        if area >= 20:
            found.append(
                {
                    "x": slices[1].start,
                    "y": slices[0].start,
                    "width": width,
                    "height": height,
                    "area": area,
                }
            )
    return found


def bbox(mask):
    ys, xs = np.nonzero(mask)
    if not len(xs):
        return None
    return [int(xs.min()), int(ys.min()), int(xs.max()) + 1, int(ys.max()) + 1]


def structural_edges(array):
    """Return major luminance edges while suppressing texture and antialiasing."""
    rgb = array.astype(np.float32)
    luminance = rgb[:, :, 0] * 0.2126 + rgb[:, :, 1] * 0.7152 + rgb[:, :, 2] * 0.0722
    smoothed = gaussian_filter(luminance, sigma=1.0)
    magnitude = np.hypot(sobel(smoothed, axis=0), sobel(smoothed, axis=1))
    return magnitude > 48.0


def bidirectional_edge_distance(ours_edges, chromium_edges):
    """Return symmetric nearest-edge distances in native CSS-pixel space.

    Row and column projections intentionally discard the other coordinate, so
    two layouts can have identical projections while their boxes occupy
    different quadrants. A distance transform retains both coordinates. Each
    engine contributes equally instead of allowing the denser edge map to
    dominate the score.
    """
    ours_count = int(ours_edges.sum())
    chromium_count = int(chromium_edges.sum())
    if ours_count == 0 and chromium_count == 0:
        return {
            "edge_bidirectional_mean_distance_px": 0.0,
            "edge_bidirectional_p95_distance_px": 0.0,
        }
    if ours_count == 0 or chromium_count == 0:
        return {
            "edge_bidirectional_mean_distance_px": None,
            "edge_bidirectional_p95_distance_px": None,
        }

    distance_to_chromium = distance_transform_edt(~chromium_edges)[ours_edges]
    distance_to_ours = distance_transform_edt(~ours_edges)[chromium_edges]
    mean_distance = (distance_to_chromium.mean() + distance_to_ours.mean()) / 2.0
    p95_distance = (
        np.percentile(distance_to_chromium, 95)
        + np.percentile(distance_to_ours, 95)
    ) / 2.0
    return {
        "edge_bidirectional_mean_distance_px": round(float(mean_distance), 6),
        "edge_bidirectional_p95_distance_px": round(float(p95_distance), 6),
    }


def pair_metrics(ours, chromium):
    if ours.shape != chromium.shape:
        return {"size_mismatch": [list(ours.shape), list(chromium.shape)]}
    delta = np.abs(ours.astype(np.int16) - chromium.astype(np.int16))
    max_channel = delta.max(axis=2)
    ours_ink = np.any(ours < 245, axis=2)
    chromium_ink = np.any(chromium < 245, axis=2)
    ours_bbox = bbox(ours_ink)
    chromium_bbox = bbox(chromium_ink)
    bbox_delta = None
    if ours_bbox and chromium_bbox:
        bbox_delta = max(abs(a - b) for a, b in zip(ours_bbox, chromium_bbox))
    height, width = ours_ink.shape
    row_projection = float(
        np.abs(ours_ink.sum(axis=1) - chromium_ink.sum(axis=1)).mean() / width
    )
    col_projection = float(
        np.abs(ours_ink.sum(axis=0) - chromium_ink.sum(axis=0)).mean() / height
    )
    # A fixed `<245` "ink" threshold is useful on white fixtures but treats a
    # uniform #eee page background as content covering the entire canvas. Major
    # blurred edges are a better structural signal for live pages: they retain
    # box boundaries and glyph rows while largely ignoring background color,
    # low-amplitude texture, and font antialiasing differences.
    ours_edges = structural_edges(ours)
    chromium_edges = structural_edges(chromium)
    ours_luminance = (
        ours[:, :, 0].astype(np.float32) * 0.2126
        + ours[:, :, 1].astype(np.float32) * 0.7152
        + ours[:, :, 2].astype(np.float32) * 0.0722
    )
    chromium_luminance = (
        chromium[:, :, 0].astype(np.float32) * 0.2126
        + chromium[:, :, 1].astype(np.float32) * 0.7152
        + chromium[:, :, 2].astype(np.float32) * 0.0722
    )
    ours_edge_bbox = bbox(ours_edges)
    chromium_edge_bbox = bbox(chromium_edges)
    edge_bbox_delta = None
    if ours_edge_bbox and chromium_edge_bbox:
        edge_bbox_delta = max(
            abs(a - b) for a, b in zip(ours_edge_bbox, chromium_edge_bbox)
        )
    ours_edge_rows = gaussian_filter1d(ours_edges.sum(axis=1).astype(float), sigma=2.0)
    chromium_edge_rows = gaussian_filter1d(
        chromium_edges.sum(axis=1).astype(float), sigma=2.0
    )
    ours_edge_cols = gaussian_filter1d(ours_edges.sum(axis=0).astype(float), sigma=2.0)
    chromium_edge_cols = gaussian_filter1d(
        chromium_edges.sum(axis=0).astype(float), sigma=2.0
    )
    edge_row_projection = float(np.abs(ours_edge_rows - chromium_edge_rows).mean() / width)
    edge_col_projection = float(np.abs(ours_edge_cols - chromium_edge_cols).mean() / height)
    edge_distance = bidirectional_edge_distance(ours_edges, chromium_edges)
    metrics = {
        "rgb_mae": round(float(delta.mean() / 255.0), 6),
        "pixels_gt_10": round(float((max_channel > 10).mean()), 6),
        "pixels_gt_50": round(float((max_channel > 50).mean()), 6),
        "ours_content_bbox": ours_bbox,
        "chromium_content_bbox": chromium_bbox,
        "content_bbox_max_delta": bbox_delta,
        "row_projection_delta": round(row_projection, 6),
        "column_projection_delta": round(col_projection, 6),
        "ours_edge_bbox": ours_edge_bbox,
        "chromium_edge_bbox": chromium_edge_bbox,
        "edge_bbox_max_delta": edge_bbox_delta,
        "edge_row_projection_delta": round(edge_row_projection, 6),
        "edge_column_projection_delta": round(edge_col_projection, 6),
        # A matching solid canvas is not evidence of rendering fidelity. Keep
        # enough per-engine signal for the paired harness to exclude blank or
        # otherwise contentless captures without discarding their raw deltas.
        "ours_luminance_stddev": round(float(ours_luminance.std()), 6),
        "chromium_luminance_stddev": round(float(chromium_luminance.std()), 6),
        "ours_structural_edge_pixels": int(ours_edges.sum()),
        "chromium_structural_edge_pixels": int(chromium_edges.sum()),
    }
    metrics.update(edge_distance)
    return metrics


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: check.py OUTDIR")
    out = Path(sys.argv[1])
    checks = json.loads((Path(__file__).with_name("checks.json")).read_text())
    failures = []
    report = {"fixtures": {}}

    for ours_path in sorted(out.glob("*.obscura.png")):
        name = ours_path.name.removesuffix(".obscura.png")
        chromium_path = out / f"{name}.chrome.png"
        if not chromium_path.is_file():
            failures.append(f"{name}: missing Chromium screenshot")
            continue
        ours = np.asarray(Image.open(ours_path).convert("RGB"))
        chromium = np.asarray(Image.open(chromium_path).convert("RGB"))
        fixture = {
            "metrics": pair_metrics(ours, chromium),
            "behavior": {"obscura": [], "chromium": []},
        }
        for engine, array in (("obscura", ours), ("chromium", chromium)):
            for check in checks.get(name, []):
                expected_count = check.get("count", 1)
                matches = []
                for component in components(array, rgb(check["color"])):
                    x_ok = "x" not in check or abs(component["x"] - check["x"]) <= 1
                    y_ok = "y" not in check or abs(component["y"] - check["y"]) <= 1
                    width_ok = "width" not in check or abs(component["width"] - check["width"]) <= 1
                    height_ok = "height" not in check or abs(component["height"] - check["height"]) <= 1
                    if x_ok and y_ok and width_ok and height_ok:
                        matches.append(component)
                passed = len(matches) >= expected_count
                fixture["behavior"][engine].append(
                    {
                        "name": check["name"],
                        "passed": passed,
                        "expected_count": expected_count,
                        "matches": matches,
                    }
                )
                if not passed:
                    failures.append(
                        f"{name} {engine}: {check['name']} expected {expected_count}, found {len(matches)}"
                    )
        report["fixtures"][name] = fixture
        metrics = fixture["metrics"]
        print(
            f"{name:24} rgb_mae={metrics.get('rgb_mae', 'size-mismatch'):>8} "
            f"p>50={metrics.get('pixels_gt_50', '-'):>8} "
            f"edge_bbox={metrics.get('edge_bbox_max_delta', '-'):>3} "
            f"edge_row={metrics.get('edge_row_projection_delta', '-'):>8} "
            f"edge_col={metrics.get('edge_column_projection_delta', '-'):>8} "
            f"edge_2d={metrics.get('edge_bidirectional_mean_distance_px', '-'):>8}"
        )

    report["failures"] = failures
    (out / "analysis.json").write_text(json.dumps(report, indent=2) + "\n")
    if failures:
        for failure in failures:
            print(f"FAILED behavior: {failure}", file=sys.stderr)
        raise SystemExit(1)
    print(f"behavior checks passed; diagnostics in {out / 'analysis.json'}")


if __name__ == "__main__":
    main()
