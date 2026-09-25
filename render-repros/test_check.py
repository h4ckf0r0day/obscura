#!/usr/bin/env python3

import unittest

import numpy as np

from check import pair_metrics


class PairMetricsTests(unittest.TestCase):
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
