#!/usr/bin/env python3

import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import numpy as np

from PIL import Image

from check import main, pair_metrics


class PairMetricsTests(unittest.TestCase):
    def test_absent_color_check_rejects_an_unexpected_component(self):
        for extra_component in (False, True):
            with self.subTest(extra_component=extra_component), tempfile.TemporaryDirectory() as directory:
                out = Path(directory)
                image = np.full((200, 500, 3), 255, dtype=np.uint8)
                image[90:120, 20:220] = [8, 127, 91]
                image[20:120, 260:460] = [112, 72, 232]
                if extra_component:
                    image[140:170, 20:60] = [232, 89, 12]
                for engine in ("obscura", "chrome"):
                    Image.fromarray(image).save(out / f"sparse-grid-scroll.{engine}.png")
                with patch.object(sys, "argv", ["check.py", str(out)]), \
                        contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    if extra_component:
                        with self.assertRaises(SystemExit) as raised:
                            main()
                        self.assertEqual(raised.exception.code, 1)
                    else:
                        main()
                report = json.loads((out / "analysis.json").read_text())
                for engine in ("obscura", "chromium"):
                    check = report["fixtures"]["sparse-grid-scroll"]["behavior"][engine][-1]
                    self.assertEqual(check["passed"], not extra_component)

    def test_bidirectional_distance_catches_projection_preserving_rearrangement(self):
        diagonal = np.full((160, 160, 3), 255, dtype=np.uint8)
        anti_diagonal = diagonal.copy()
        diagonal[20:50, 20:50] = 0
        diagonal[100:130, 100:130] = 0
        anti_diagonal[20:50, 100:130] = 0
        anti_diagonal[100:130, 20:50] = 0

        metrics = pair_metrics(diagonal, anti_diagonal)

        self.assertAlmostEqual(metrics["edge_row_projection_delta"], 0.0)
        self.assertAlmostEqual(metrics["edge_column_projection_delta"], 0.0)
        self.assertGreater(metrics["edge_bidirectional_mean_distance_px"], 20.0)

    def test_identical_images_have_zero_bidirectional_distance(self):
        image = np.full((80, 80, 3), 255, dtype=np.uint8)
        image[20:60, 25:55] = 0

        metrics = pair_metrics(image, image)

        self.assertEqual(metrics["edge_bidirectional_mean_distance_px"], 0.0)
        self.assertEqual(metrics["edge_bidirectional_p95_distance_px"], 0.0)

    def test_solid_pair_records_no_visual_signal(self):
        image = np.zeros((80, 80, 3), dtype=np.uint8)

        metrics = pair_metrics(image, image)

        self.assertEqual(metrics["ours_luminance_stddev"], 0.0)
        self.assertEqual(metrics["chromium_luminance_stddev"], 0.0)
        self.assertEqual(metrics["ours_structural_edge_pixels"], 0)
        self.assertEqual(metrics["chromium_structural_edge_pixels"], 0)


if __name__ == "__main__":
    unittest.main()
