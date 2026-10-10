#!/usr/bin/env python3

import hashlib
import importlib.util
import json
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).with_name("paired-corpus.py")
SPEC = importlib.util.spec_from_file_location("paired_corpus", SCRIPT)
paired_corpus = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(paired_corpus)


class MediaEnvironmentTests(unittest.TestCase):
    def test_canonical_light_default_motion_environment_matches(self):
        self.assertTrue(
            paired_corpus.media_matches_configured(
                dict(paired_corpus.EXPECTED_MEDIA_MATCHES)
            )
        )

    def test_dark_or_reduced_environment_is_rejected(self):
        dark = dict(paired_corpus.EXPECTED_MEDIA_MATCHES)
        dark["prefers_color_scheme_light"] = False
        dark["prefers_color_scheme_dark"] = True
        self.assertFalse(paired_corpus.media_matches_configured(dark))

        reduced = dict(paired_corpus.EXPECTED_MEDIA_MATCHES)
        reduced["prefers_reduced_motion_no_preference"] = False
        reduced["prefers_reduced_motion_reduce"] = True
        self.assertFalse(paired_corpus.media_matches_configured(reduced))


class ControlledScrollTests(unittest.TestCase):
    def test_bottom_expression_resolves_live_document_height(self):
        expression = paired_corpus.scroll_eval_expression((12, "bottom"))
        self.assertIn("const requestedX=12", expression)
        self.assertIn(
            "requestedY=document.documentElement.scrollHeight", expression
        )
        self.assertIn("window.scrollTo(requestedX,requestedY)", expression)
        self.assertIn("preInitialActual", expression)
        self.assertIn("postInitialActual", expression)
        self.assertNotIn("behavior:'instant'", expression)

    def test_chromium_reassert_records_settled_and_final_offsets(self):
        class FakePage:
            def __init__(self):
                self.calls = []

            def evaluate(self, expression, argument):
                self.calls.append((expression, argument))
                return {
                    "requested": {"x": 4, "y": 300},
                    "pre_reassert_actual": {"x": 4, "y": 287},
                    "final_actual": {"x": 4, "y": 300},
                    "reassert_behavior": "instant",
                }

        page = FakePage()
        result = paired_corpus.reassert_chromium_controlled_scroll(
            page, (4, 300)
        )
        self.assertEqual(result["pre_reassert_actual"]["y"], 287)
        self.assertEqual(result["final_actual"]["y"], 300)
        expression, argument = page.calls[0]
        self.assertEqual(argument, [4, 300])
        self.assertIn('behavior: "instant"', expression)
        self.assertLess(
            expression.index("beforeReassert"),
            expression.index("window.scrollTo({"),
        )

    def test_capture_report_uses_post_settle_state(self):
        stdout = (
            "diagnostic\n"
            '{"evaluation":"{\\"sampled_phase\\":'
            '\\"capture-boundary-before-screenshot\\",'
            '\\"requested\\":null,\\"controlled_scroll\\":null}",'
            '"controlledScroll":{"requested":{"x":0,"y":2702},'
            '"preInitialActual":{"x":0,"y":0},'
            '"postInitialActual":{"x":0,"y":12},'
            '"initialBehavior":"authored",'
            '"initialPhase":"before-controlled-scroll-settle",'
            '"preReassertActual":{"x":0,"y":1791},'
            '"finalReassertActual":{"x":0,"y":1802},'
            '"behavior":"instant",'
            '"phase":"immediately-before-capture-state-and-screenshot"},'
            '"captureState":{"scrollX":0,"scrollY":1802,'
            '"innerWidth":1280,"innerHeight":900,'
            '"scrollWidth":1291,"scrollHeight":2702}}\n'
        )
        state = paired_corpus.parse_obscura_scroll_report(stdout)
        self.assertEqual(state["requested"], {"x": 0, "y": 2702})
        self.assertEqual(
            state["pre_reassert_actual"], {"x": 0, "y": 1791}
        )
        self.assertEqual(state["pre_initial_actual"], {"x": 0, "y": 0})
        self.assertEqual(state["post_initial_actual"], {"x": 0, "y": 12})
        self.assertEqual(
            state["final_reassert_actual"], {"x": 0, "y": 1802}
        )
        self.assertEqual(state["actual"], {"x": 0, "y": 1802})
        self.assertEqual(state["reassert_behavior"], "instant")
        self.assertEqual(state["content"]["height"], 2702)
        self.assertEqual(
            state["sampled_phase"], paired_corpus.CAPTURE_BOUNDARY_PHASE
        )

    def test_obscura_capture_exports_final_scroll_request_to_cli(self):
        environment = paired_corpus.obscura_environment(1280, 720)
        paired_corpus.with_controlled_scroll_environment(
            environment, (12, "bottom")
        )
        self.assertEqual(environment["OBSCURA_SHOT_SCROLL_X"], "12")
        self.assertEqual(environment["OBSCURA_SHOT_SCROLL_Y"], "bottom")
        self.assertEqual(environment["OBSCURA_SHOT_EVAL_AT_CAPTURE"], "1")
        self.assertEqual(environment["OBSCURA_SHOT_RESOURCE_WARMUP"], "1")
        self.assertNotIn("OBSCURA_SHOT_ANIMATION_TIME_MS", environment)

    def test_obscura_capture_exports_explicit_animation_sample(self):
        environment = paired_corpus.obscura_environment(1280, 720, 0)
        self.assertEqual(environment["OBSCURA_SHOT_ANIMATION_TIME_MS"], "0")

    def test_paired_state_expression_is_read_only_at_capture_boundary(self):
        expression = paired_corpus.obscura_state_eval_expression(
            None,
            [".card"],
            sampled_phase=paired_corpus.CAPTURE_BOUNDARY_PHASE,
        )
        self.assertIn(
            f'sampled_phase:"{paired_corpus.CAPTURE_BOUNDARY_PHASE}"',
            expression,
        )
        self.assertIn("geometry_probes:geometryProbes", expression)
        self.assertNotIn("window.scrollTo", expression)

    def test_every_capture_expression_samples_page_state_without_scrolling(self):
        expression = paired_corpus.obscura_state_eval_expression(None)
        self.assertIn("outer_html_fnv1a32", expression)
        self.assertIn("data-obscura-external-stylesheets", expression)
        self.assertIn("normalized_outer_html_fnv1a32", expression)
        self.assertIn("body_text_fnv1a32", expression)
        self.assertIn("body.textContent", expression)
        self.assertNotIn("body.innerText", expression)
        self.assertIn("injectedStyles.reduce", expression)
        self.assertNotIn("cloneNode", expression)
        self.assertNotIn("window.scrollTo", expression)

    def test_capture_report_keeps_dom_state_and_authoritative_geometry(self):
        stdout = (
            '{"evaluation":"{\\"sampled_phase\\":'
            '\\"capture-boundary-before-screenshot\\",'
            '\\"document\\":{\\"ready_state\\":\\"complete\\",'
            '\\"element_count\\":7,\\"outer_html_fnv1a32\\":\\"12345678\\"},'
            '\\"geometry\\":{\\"document_scroll_height\\":999}}",'
            '"captureState":{"scrollX":3,"scrollY":40,'
            '"innerWidth":640,"innerHeight":480,'
            '"scrollWidth":650,"scrollHeight":1200}}\n'
        )
        state = paired_corpus.parse_obscura_capture_report(stdout)
        self.assertEqual(state["document"]["element_count"], 7)
        self.assertEqual(state["geometry"]["scroll_y"], 40)
        self.assertEqual(state["geometry"]["document_scroll_height"], 1200)
        self.assertTrue(state["state_and_screenshot_share_capture_boundary"])

    def test_legacy_pre_settle_report_is_not_relabelled_as_capture_state(self):
        stdout = (
            '{"evaluation":"{\\"sampled_phase\\":'
            '\\"before-cli-post-eval-settle\\",'
            '\\"document\\":{},\\"geometry\\":{}}",'
            '"captureState":{"scrollX":0,"scrollY":0,'
            '"innerWidth":640,"innerHeight":480,'
            '"scrollWidth":640,"scrollHeight":480}}\n'
        )
        state = paired_corpus.parse_obscura_capture_report(stdout)
        self.assertEqual(
            state["sampled_phase"], "before-cli-post-eval-settle"
        )
        self.assertEqual(
            state["screenshot_sampled_phase"],
            paired_corpus.CAPTURE_BOUNDARY_PHASE,
        )
        self.assertFalse(state["state_and_screenshot_share_capture_boundary"])

    def test_page_state_comparison_reports_provenance_and_geometry_deltas(self):
        obscura = {
            "document": {
                "ready_state": "complete",
                "element_count": 9,
                "outer_html_utf16": 100,
                "normalized_outer_html_utf16": 90,
                "body_text_utf16": 30,
                "outer_html_fnv1a32": "aaaaaaaa",
                "normalized_outer_html_fnv1a32": "dddddddd",
                "body_text_fnv1a32": "bbbbbbbb",
            },
            "geometry": {"document_scroll_height": 1200, "scroll_y": 300},
        }
        chromium = {
            "document": {
                "ready_state": "complete",
                "element_count": 7,
                "outer_html_utf16": 95,
                "normalized_outer_html_utf16": 90,
                "body_text_utf16": 30,
                "outer_html_fnv1a32": "cccccccc",
                "normalized_outer_html_fnv1a32": "dddddddd",
                "body_text_fnv1a32": "bbbbbbbb",
            },
            "geometry": {"document_scroll_height": 1000, "scroll_y": 250},
        }
        comparison = paired_corpus.compare_page_states(obscura, chromium)
        self.assertTrue(comparison["ready_state_equal"])
        self.assertEqual(comparison["element_count_delta"], 2)
        self.assertFalse(comparison["outer_html_fingerprint_equal"])
        self.assertTrue(comparison["normalized_outer_html_fingerprint_equal"])
        self.assertEqual(comparison["normalized_outer_html_utf16_delta"], 0)
        self.assertTrue(comparison["body_text_fingerprint_equal"])
        self.assertEqual(
            comparison["geometry_delta"]["document_scroll_height"], 200
        )
        self.assertEqual(comparison["geometry_delta"]["scroll_y"], 50)

    def test_scroll_y_parser_accepts_bottom_and_integer(self):
        self.assertEqual(paired_corpus.parse_scroll_y("bottom"), "bottom")
        self.assertEqual(paired_corpus.parse_scroll_y("-20"), -20)


class StateComparabilityTests(unittest.TestCase):
    @staticmethod
    def state(element_count, body_text_utf16, probe_counts, **extra):
        state = {
            "url": "https://fixture.test/delayed-route",
            "document": {
                "ready_state": "complete",
                "element_count": element_count,
                "body_text_utf16": body_text_utf16,
                # Deliberately unrelated: hashes must not drive classification.
                "outer_html_fnv1a32": extra.pop("outer_hash", "aaaaaaaa"),
                "body_text_fnv1a32": extra.pop("text_hash", "bbbbbbbb"),
            },
            "geometry_probes": [
                {
                    "selector": f".probe-{index}",
                    "valid": True,
                    "count": count,
                    "rects": [],
                }
                for index, count in enumerate(probe_counts)
            ],
            "state_and_screenshot_share_capture_boundary": True,
        }
        state.update(extra)
        return state

    def test_delayed_route_at_zero_settle_is_different_live_state_and_not_fidelity(self):
        pending_route = self.state(
            41,
            921,
            [0, 1, 0, 0, 0, 0, 0],
        )
        loaded_route = self.state(
            397,
            3924,
            [2, 1, 7, 4, 18, 3, 2],
            outer_hash="cccccccc",
            text_hash="dddddddd",
        )

        comparison = paired_corpus.classify_state_comparability(
            pending_route,
            loaded_route,
            {"stable": True},
        )
        fidelity = paired_corpus.classify_fidelity_metric(
            "cold-load-latency",
            comparison["state_comparable"],
            metrics_present=True,
        )

        self.assertFalse(comparison["state_comparable"])
        self.assertEqual(comparison["classification"], "different-live-state")
        self.assertGreaterEqual(len(comparison["gross_provenance_signals"]), 2)
        self.assertFalse(fidelity["fidelity_metric_valid"])
        self.assertIn("cold-load-latency-mode", fidelity["exclusion_reasons"])

    def test_delayed_route_after_settle_is_comparable_despite_hashes(self):
        obscura = self.state(
            399,
            3910,
            [2, 1, 7, 4, 18, 3, 2],
            outer_hash="11111111",
            text_hash="22222222",
        )
        chromium = self.state(
            397,
            3924,
            [2, 1, 7, 4, 18, 3, 2],
            outer_hash="ffffffff",
            text_hash="eeeeeeee",
        )

        comparison = paired_corpus.classify_state_comparability(
            obscura,
            chromium,
            {"stable": True},
        )
        fidelity = paired_corpus.classify_fidelity_metric(
            "representative-fidelity",
            comparison["state_comparable"],
            metrics_present=True,
        )

        self.assertTrue(comparison["state_comparable"])
        self.assertEqual(comparison["classification"], "comparable")
        self.assertFalse(comparison["hashes_used_for_classification"])
        self.assertTrue(fidelity["fidelity_metric_valid"])

    def test_matching_contentless_canvases_are_not_fidelity_evidence(self):
        metrics = {
            "pixels_gt_50": 0.0,
            "ours_luminance_stddev": 0.0,
            "chromium_luminance_stddev": 0.0,
            "ours_structural_edge_pixels": 0,
            "chromium_structural_edge_pixels": 0,
        }

        fidelity = paired_corpus.classify_fidelity_metric(
            "representative-fidelity",
            True,
            metrics_present=True,
            metrics=metrics,
        )

        self.assertFalse(fidelity["fidelity_metric_valid"])
        self.assertIn("contentless-image-pair", fidelity["exclusion_reasons"])

    def test_live_and_deterministic_modes_with_same_dom_provenance_are_comparable(self):
        live = self.state(
            210,
            1700,
            [4, 2, 12, 3],
            animation_sampling={"mode": "live-wall-clock"},
        )
        deterministic = self.state(
            211,
            1692,
            [4, 2, 12, 3],
            animation_sampling={
                "mode": "deterministic-active-web-animations",
                "sample_ms": 0,
            },
        )

        comparison = paired_corpus.classify_state_comparability(
            live,
            deterministic,
            {"stable": True},
        )

        self.assertTrue(comparison["state_comparable"])
        self.assertEqual(comparison["gross_provenance_signals"], [])

    def test_capture_boundary_instability_excludes_otherwise_matching_state(self):
        obscura = self.state(100, 800, [2, 4, 1])
        chromium = self.state(100, 800, [2, 4, 1])

        comparison = paired_corpus.classify_state_comparability(
            obscura,
            chromium,
            {"stable": False},
        )

        self.assertFalse(comparison["state_comparable"])
        self.assertEqual(
            comparison["classification"], "capture-boundary-unstable"
        )

    def test_one_moderate_provenance_difference_does_not_overreject(self):
        obscura = self.state(70, 800, [2, 4, 1])
        chromium = self.state(100, 800, [2, 4, 1])

        comparison = paired_corpus.classify_state_comparability(
            obscura,
            chromium,
            {"stable": True},
        )

        self.assertTrue(comparison["state_comparable"])


class GeometryProbeTests(unittest.TestCase):
    @staticmethod
    def chromium_state():
        return {
            "document": {
                "outer_html_sha256": "outer",
                "body_text_sha256": "text",
            },
            "geometry_probes": [],
        }

    class FakePage:
        def __init__(self, state):
            self.state = state
            self.calls = []

        def evaluate(self, expression, *args):
            self.calls.append((expression, args))
            return self.state

    def test_default_state_expressions_do_not_include_probe_work(self):
        obscura_expression = paired_corpus.obscura_state_eval_expression(None)
        self.assertNotIn("sampleGeometrySelector", obscura_expression)
        self.assertNotIn("geometry_probes", obscura_expression)
        self.assertIn("feature_probes:featureProbes", obscura_expression)

        page = self.FakePage(self.chromium_state())
        paired_corpus.capture_chromium_state(page)
        self.assertEqual(len(page.calls), 1)
        expression, args = page.calls[0]
        self.assertEqual(args, ())
        self.assertNotIn("sampleGeometrySelector", expression)
        self.assertNotIn("geometry_probes", expression)
        self.assertIn("feature_probes: featureProbes", expression)
        self.assertNotIn("async ", expression)
        self.assertNotIn("await ", expression)
        self.assertNotIn("crypto.subtle", expression)
        self.assertIn(
            f'sampled_phase: "{paired_corpus.CAPTURE_BOUNDARY_PHASE}"',
            expression,
        )

    def test_feature_discovery_is_bounded_selector_free_and_avoids_nested_scroll(self):
        expression = paired_corpus.feature_probe_javascript("no quads")
        self.assertIn(
            f"Math.min(featureProbeElements.length,{paired_corpus.FEATURE_PROBE_SCAN_LIMIT})",
            expression,
        )
        self.assertIn(
            f"category.candidates.length<{paired_corpus.FEATURE_PROBE_CANDIDATE_LIMIT}",
            expression,
        )
        self.assertIn("style.transform!=='none'", expression)
        self.assertIn("style.translate!=='none'", expression)
        self.assertIn("style.rotate!=='none'", expression)
        self.assertIn("style.scale!=='none'", expression)
        self.assertIn("style.perspective!=='none'", expression)
        self.assertIn("style.textOverflow!=='clip'", expression)
        self.assertIn("style.webkitLineClamp!=='none'", expression)
        self.assertIn("box_quads:null", expression)
        self.assertIn("comparison_index:comparisonIndex", expression)
        self.assertIn("data-obscura-external-stylesheets", expression)
        self.assertNotIn("querySelector", expression)
        self.assertNotIn("scrollWidth", expression)
        self.assertNotIn("scrollHeight", expression)
        self.assertNotIn("clientWidth", expression)
        self.assertNotIn("clientHeight", expression)

    def test_obscura_records_quad_capability_instead_of_fabricated_corners(self):
        expression = paired_corpus.obscura_state_eval_expression(None)
        self.assertIn(
            "Obscura CLI capture does not expose node-scoped CDP",
            expression,
        )
        self.assertIn("box_quads:null", expression)
        self.assertNotIn("rect.left,rect.top,rect.right", expression)

    def test_chromium_cdp_attaches_content_and_border_quads_once_per_element(self):
        class FakeSession:
            def __init__(self):
                self.calls = []

            def send(self, method, params):
                self.calls.append((method, params))
                if method == "Runtime.evaluate":
                    return {"result": {"objectId": "candidate-7"}}
                if method == "DOM.getBoxModel":
                    return {
                        "model": {
                            "content": [1, 2, 3, 2, 3, 4, 1, 4],
                            "border": [0, 1, 4, 1, 4, 5, 0, 5],
                        }
                    }
                return {}

        transform = {"dom_index": 7, "box_quads": None}
        truncation = {"dom_index": 7, "box_quads": None}
        state = {
            "feature_probes": {
                "categories": {
                    "transform": {"candidates": [transform]},
                    "text_truncation": {"candidates": [truncation]},
                }
            }
        }
        session = FakeSession()
        paired_corpus.attach_chromium_box_quads(session, state)
        methods = [method for method, _ in session.calls]
        self.assertEqual(methods.count("Runtime.evaluate"), 1)
        self.assertEqual(methods.count("DOM.getBoxModel"), 1)
        self.assertEqual(methods.count("Runtime.releaseObjectGroup"), 1)
        self.assertEqual(
            transform["box_quads"]["content"],
            [1, 2, 3, 2, 3, 4, 1, 4],
        )
        self.assertEqual(transform["box_quads"], truncation["box_quads"])
        capability = state["feature_probes"]["box_quads"]
        self.assertTrue(capability["available"])
        self.assertEqual(capability["attempted_candidates"], 1)
        self.assertEqual(capability["captured_candidates"], 1)
        self.assertEqual(capability["coordinate_space"], "viewport-css-px")

    def test_missing_chromium_cdp_session_leaves_quads_null(self):
        candidate = {"dom_index": 2, "box_quads": None}
        state = {
            "feature_probes": {
                "categories": {"transform": {"candidates": [candidate]}}
            }
        }
        paired_corpus.prepare_chromium_feature_probes(state, None)
        self.assertIsNone(candidate["box_quads"])
        capability = state["feature_probes"]["box_quads"]
        self.assertFalse(capability["available"])
        self.assertIsNone(capability["source"])
        self.assertIn("did not receive a CDP session", capability["reason"])

    def test_chromium_snapshot_paints_before_host_hashing(self):
        events = []
        first_state = {
            "_hash_sources": {
                "dom": "<html></html>",
                "normalized_dom": "<html></html>",
                "body_text": "hello",
            },
            "document": {},
        }
        second_state = {
            "_hash_sources": {
                "dom": "<html></html>",
                "normalized_dom": "<html></html>",
                "body_text": "hello",
            },
            "document": {},
        }

        class OrderedPage:
            def __init__(self):
                self.states = [first_state, second_state]

            def evaluate(self, expression, *args):
                events.append("evaluate")
                return self.states.pop(0)

            def screenshot(self, **kwargs):
                events.append("screenshot")

        real_sha256 = hashlib.sha256

        def ordered_sha256(value):
            events.append("sha256")
            return real_sha256(value)

        with mock.patch.object(
            paired_corpus.hashlib,
            "sha256",
            side_effect=ordered_sha256,
        ):
            captured, boundary = paired_corpus.capture_chromium_image(
                OrderedPage(), Path("/tmp/not-written.png")
            )

        self.assertEqual(
            events,
            ["evaluate", "screenshot", "evaluate", "sha256", "sha256", "sha256"],
        )
        self.assertTrue(boundary["stable"])
        self.assertNotIn("_hash_sources", captured)
        self.assertEqual(
            captured["document"]["outer_html_sha256"],
            hashlib.sha256(b"<html></html>").hexdigest(),
        )
        self.assertEqual(
            captured["document"]["normalized_outer_html_sha256"],
            hashlib.sha256(b"<html></html>").hexdigest(),
        )
        self.assertEqual(
            captured["document"]["body_text_sha256"],
            hashlib.sha256(b"hello").hexdigest(),
        )

    def test_chromium_resource_warmup_discards_one_shot_then_yields(self):
        events = []

        class WarmupPage:
            def screenshot(self, **kwargs):
                events.append(("screenshot", kwargs))
                return b"discarded"

            def wait_for_timeout(self, timeout):
                events.append(("wait", timeout))

        report = paired_corpus.warm_chromium_capture(WarmupPage())
        self.assertEqual([event[0] for event in events], ["screenshot", "wait"])
        self.assertEqual(events[1], ("wait", 1))
        self.assertNotIn("path", events[0][1])
        self.assertEqual(report["discardedShots"], 1)
        self.assertEqual(
            report["phase"], paired_corpus.RESOURCE_WARMUP_PHASE
        )

    def test_repeatable_selectors_are_passed_safely_in_one_state_expression(self):
        selectors = ["header nav a", '[data-label="a\\\"b"]', "["]
        obscura_expression = paired_corpus.obscura_state_eval_expression(
            (0, 25), selectors
        )
        encoded = json.dumps(selectors, ensure_ascii=True, separators=(",", ":"))
        self.assertIn(encoded, obscura_expression)
        self.assertIn("sampleGeometrySelector", obscura_expression)
        self.assertIn("catch(error)", obscura_expression)
        self.assertIn("geometry_probes:geometryProbes", obscura_expression)

        page = self.FakePage(self.chromium_state())
        paired_corpus.capture_chromium_state(page, selectors)
        self.assertEqual(len(page.calls), 1)
        expression, args = page.calls[0]
        self.assertEqual(args, (selectors,))
        self.assertIn("querySelectorAll(selector)", expression)
        self.assertIn("sampleGeometryDom(element)", expression)
        self.assertIn("subtree_element_count", expression)
        self.assertIn("parent_index", expression)
        self.assertIn("nodes.length<80", expression)
        self.assertNotIn("Array.from(element.querySelectorAll('*'))", expression)
        self.assertIn("catch(error)", expression)
        self.assertIn("font_family:style.fontFamily", expression)
        self.assertIn("line_height:style.lineHeight", expression)
        self.assertIn("white_space:style.whiteSpace", expression)
        self.assertIn("webkit_line_clamp:style.webkitLineClamp", expression)
        self.assertIn("webkit_box_orient:style.webkitBoxOrient", expression)
        self.assertIn("direction:style.direction", expression)
        self.assertIn("unicode_bidi:style.unicodeBidi", expression)
        self.assertIn(
            "grid_template_columns:style.gridTemplateColumns", expression
        )
        self.assertIn("border_left_style:style.borderLeftStyle", expression)
        self.assertIn("object_fit:style.objectFit", expression)
        self.assertIn("content_visibility:style.contentVisibility", expression)
        self.assertIn("transform_origin:style.transformOrigin", expression)
        self.assertIn("transform_box:style.transformBox", expression)
        self.assertIn("translate:style.translate", expression)
        self.assertIn("rotate:style.rotate", expression)
        self.assertIn("scale:style.scale", expression)
        self.assertIn("perspective:style.perspective", expression)
        self.assertIn("geometry_probes: geometryProbes", expression)
        self.assertIn(
            f'sampled_phase: "{paired_corpus.CAPTURE_BOUNDARY_PHASE}"',
            expression,
        )

    def test_probe_comparison_reports_raw_deltas_and_invalid_errors(self):
        obscura = {
            "geometry_probes": [
                {
                    "selector": ".card",
                    "valid": True,
                    "count": 2,
                    "rects": [
                        {
                            "x": 11,
                            "y": 18,
                            "width": 100,
                            "height": 40,
                            "visible": True,
                            "computed": {
                                "display": "grid",
                                "width": "100px",
                                "align_items": "stretch",
                            },
                        }
                    ],
                    "error": None,
                },
                {
                    "selector": "[",
                    "valid": False,
                    "count": None,
                    "rects": [],
                    "error": {"name": "SyntaxError", "message": "invalid selector"},
                },
            ]
        }
        chromium = {
            "geometry_probes": [
                {
                    "selector": ".card",
                    "valid": True,
                    "count": 1,
                    "rects": [
                        {
                            "x": 10,
                            "y": 20,
                            "width": 98,
                            "height": 40,
                            "visible": False,
                            "computed": {
                                "display": "grid",
                                "width": "98px",
                                "align_items": "normal",
                            },
                        }
                    ],
                    "error": None,
                },
                {
                    "selector": "[",
                    "valid": False,
                    "count": None,
                    "rects": [],
                    "error": {"name": "SyntaxError", "message": "invalid selector"},
                },
            ]
        }

        comparison = paired_corpus.compare_geometry_probes(obscura, chromium)
        self.assertEqual(comparison[0]["counts"]["delta"], 1)
        self.assertFalse(comparison[0]["geometry_verdict_valid"])
        self.assertIn(
            "target-count-mismatch",
            comparison[0]["geometry_verdict_exclusion_reasons"],
        )
        self.assertEqual(
            comparison[0]["rect_deltas"][0]["delta"],
            {"x": 1, "y": -2, "width": 2, "height": 0},
        )
        self.assertEqual(
            comparison[0]["rect_deltas"][0]["visibility"],
            {"obscura": True, "chromium": False},
        )
        self.assertEqual(
            comparison[0]["rect_deltas"][0]["computed_difference_count"], 2
        )
        self.assertEqual(
            comparison[0]["rect_deltas"][0]["computed_differences"],
            {
                "align_items": {"obscura": "stretch", "chromium": "normal"},
                "width": {"obscura": "100px", "chromium": "98px"},
            },
        )
        self.assertEqual(comparison[1]["valid"], {"obscura": False, "chromium": False})
        self.assertFalse(comparison[1]["geometry_verdict_valid"])
        self.assertIsNone(comparison[1]["counts"]["delta"])
        self.assertEqual(comparison[1]["rects_compared"], 0)

    def test_geometry_verdict_accepts_matching_bounded_subtrees(self):
        def descriptor(root_classes):
            return {
                "subtree_element_count": 2,
                "subtree_truncated": False,
                "subtree": [
                    {
                        "index": 0,
                        "parent_index": None,
                        "tag": "div",
                        "id": "card",
                        "class_name": root_classes,
                        "child_element_count": 1,
                    },
                    {
                        "index": 1,
                        "parent_index": 0,
                        "tag": "img",
                        "id": "",
                        "class_name": "hero",
                        "child_element_count": 0,
                    },
                ],
            }

        def state(classes, x):
            return {
                "geometry_probes": [
                    {
                        "selector": "#card",
                        "valid": True,
                        "count": 1,
                        "rect_limit": 200,
                        "rects": [
                            {
                                "x": x,
                                "y": 20,
                                "width": 100,
                                "height": 40,
                                "dom": descriptor(classes),
                            }
                        ],
                    }
                ]
            }

        comparison = paired_corpus.compare_geometry_probes(
            state("featured card", 11), state("card featured", 10)
        )
        self.assertTrue(comparison[0]["geometry_verdict_valid"])
        self.assertEqual(comparison[0]["geometry_verdict_exclusion_reasons"], [])
        self.assertTrue(comparison[0]["rect_deltas"][0]["geometry_delta_valid"])
        self.assertEqual(paired_corpus.geometry_verdict_exclusions(comparison), [])

    def test_geometry_target_accepts_repeated_angular_id_salt_variants(self):
        def descriptor(salt):
            return {
                "subtree_element_count": 3,
                "subtree_truncated": False,
                "subtree": [
                    {
                        "index": 0,
                        "parent_index": None,
                        "tag": "div",
                        "id": f"ng-tab-{salt}",
                        "class_name": "nav nav-tabs",
                        "child_element_count": 2,
                    },
                    {
                        "index": 1,
                        "parent_index": 0,
                        "tag": "button",
                        "id": f"ng-tab-{salt}-label",
                        "class_name": "nav-link active",
                        "child_element_count": 0,
                    },
                    {
                        "index": 2,
                        "parent_index": 0,
                        "tag": "div",
                        "id": f"ng-tab-{salt}-panel",
                        "class_name": "tab-pane active",
                        "child_element_count": 0,
                    },
                ],
            }

        def state(salt, x):
            return {
                "geometry_probes": [
                    {
                        "selector": ".nav-tabs",
                        "valid": True,
                        "count": 1,
                        "rect_limit": 200,
                        "rects": [
                            {
                                "x": x,
                                "y": 10,
                                "width": 300,
                                "height": 80,
                                "dom": descriptor(salt),
                            }
                        ],
                    }
                ]
            }

        probe = paired_corpus.compare_geometry_probes(
            state("7", 12), state("19", 10)
        )[0]
        self.assertTrue(probe["geometry_verdict_valid"])
        self.assertEqual(probe["geometry_verdict_exclusion_reasons"], [])
        self.assertTrue(probe["rect_deltas"][0]["geometry_delta_valid"])
        self.assertEqual(probe["rect_deltas"][0]["delta"]["x"], 2)
        comparison = probe["rect_deltas"][0]["target_subtree_comparability"]
        self.assertTrue(comparison["comparable"])
        self.assertTrue(comparison["topology_equal"])
        self.assertEqual(
            comparison["id_comparison"]["normalized_mismatch_count"], 3
        )
        self.assertEqual(comparison["id_comparison"]["semantic_mismatch_count"], 0)
        self.assertTrue(
            all(
                mismatch["normalized_as_volatile"]
                for mismatch in comparison["id_comparison"]["mismatches"]
            )
        )
        self.assertEqual(comparison["obscura"]["nodes"][0]["id"], "ng-tab-7")
        self.assertEqual(
            comparison["chromium"]["nodes"][0]["id"], "ng-tab-19"
        )
        self.assertEqual(
            comparison["id_comparison"]["mismatches"][2]["variance"][
                "normalized_fingerprint"
            ],
            "ng-tab-<volatile-id-salt>-panel",
        )

    def test_geometry_target_rejects_one_off_generated_id_salt(self):
        def descriptor(salt):
            return {
                "subtree_element_count": 1,
                "subtree_truncated": False,
                "subtree": [
                    {
                        "index": 0,
                        "parent_index": None,
                        "tag": "div",
                        "id": f"ng-tab-{salt}",
                        "class_name": "nav-tabs",
                        "child_element_count": 0,
                    }
                ],
            }

        comparison = paired_corpus.compare_geometry_dom_structures(
            descriptor("7"), descriptor("19")
        )
        self.assertFalse(comparison["comparable"])
        self.assertEqual(
            comparison["id_comparison"]["normalized_mismatch_count"], 0
        )
        self.assertEqual(comparison["id_comparison"]["semantic_mismatch_count"], 1)

    def test_geometry_target_rejects_repeated_semantic_id_changes(self):
        def descriptor(section):
            return {
                "subtree_element_count": 2,
                "subtree_truncated": False,
                "subtree": [
                    {
                        "index": 0,
                        "parent_index": None,
                        "tag": "section",
                        "id": f"{section}-settings",
                        "class_name": "settings",
                        "child_element_count": 1,
                    },
                    {
                        "index": 1,
                        "parent_index": 0,
                        "tag": "button",
                        "id": f"{section}-save",
                        "class_name": "button primary",
                        "child_element_count": 0,
                    },
                ],
            }

        comparison = paired_corpus.compare_geometry_dom_structures(
            descriptor("account"), descriptor("billing")
        )
        self.assertFalse(comparison["comparable"])
        self.assertTrue(comparison["topology_equal"])
        self.assertEqual(
            comparison["classification"], "different-target-structure"
        )
        self.assertIn("target-subtree-structure-mismatch", comparison["reasons"])
        self.assertEqual(
            comparison["id_comparison"]["normalized_mismatch_count"], 0
        )
        self.assertEqual(comparison["id_comparison"]["semantic_mismatch_count"], 2)

    def test_geometry_verdict_excludes_different_dynamic_subtree_only(self):
        def state(child_class):
            return {
                "geometry_probes": [
                    {
                        "selector": "#ad",
                        "valid": True,
                        "count": 1,
                        "rect_limit": 200,
                        "rects": [
                            {
                                "x": 1,
                                "y": 2,
                                "width": 300,
                                "height": 200,
                                "dom": {
                                    "subtree_element_count": 2,
                                    "subtree_truncated": False,
                                    "subtree": [
                                        {
                                            "index": 0,
                                            "parent_index": None,
                                            "tag": "div",
                                            "id": "ad",
                                            "class_name": "",
                                            "child_element_count": 1,
                                        },
                                        {
                                            "index": 1,
                                            "parent_index": 0,
                                            "tag": "a",
                                            "id": "",
                                            "class_name": child_class,
                                            "child_element_count": 0,
                                        },
                                    ],
                                },
                            }
                        ],
                    }
                ]
            }

        comparison = paired_corpus.compare_geometry_probes(
            state("ad-image-large"), state("ad-image-small")
        )
        probe = comparison[0]
        self.assertFalse(probe["geometry_verdict_valid"])
        self.assertIn(
            "target-subtree-structure-mismatch",
            probe["geometry_verdict_exclusion_reasons"],
        )
        self.assertFalse(probe["rect_deltas"][0]["geometry_delta_valid"])
        self.assertEqual(
            paired_corpus.geometry_verdict_exclusions(comparison)[0]["selector"],
            "#ad",
        )
        summary = paired_corpus.summarize_geometry_verdicts(comparison)
        self.assertEqual(summary["valid_selectors"], 0)
        self.assertEqual(summary["excluded_selectors"], 1)
        self.assertFalse(summary["all_selectors_valid"])
        # Selector-scoped target mismatch does not poison unrelated page pixels.
        fidelity = paired_corpus.classify_fidelity_metric(
            "representative-fidelity", True, metrics_present=True
        )
        self.assertTrue(fidelity["fidelity_metric_valid"])

    def test_legacy_geometry_report_is_insufficient_for_verdict(self):
        legacy_dom = {
            "tag": "div",
            "id": "card",
            "class_name": "card",
            "child_element_count": 0,
            "children": [],
        }
        state = {
            "geometry_probes": [
                {
                    "selector": "#card",
                    "valid": True,
                    "count": 1,
                    "rects": [
                        {
                            "x": 0,
                            "y": 0,
                            "width": 20,
                            "height": 20,
                            "dom": legacy_dom,
                        }
                    ],
                }
            ]
        }
        comparison = paired_corpus.compare_geometry_probes(state, state)
        self.assertFalse(comparison[0]["geometry_verdict_valid"])
        self.assertIn(
            "target-subtree-descriptor-unavailable",
            comparison[0]["geometry_verdict_exclusion_reasons"],
        )

    def test_truncated_equal_subtrees_are_insufficient_for_verdict(self):
        dom = {
            "subtree_element_count": 100,
            "subtree_truncated": True,
            "subtree": [
                {
                    "index": 0,
                    "parent_index": None,
                    "tag": "div",
                    "id": "target",
                    "class_name": "same-prefix",
                    "child_element_count": 99,
                }
            ],
        }
        state = {
            "geometry_probes": [
                {
                    "selector": "#target",
                    "valid": True,
                    "count": 1,
                    "rects": [
                        {
                            "x": 0,
                            "y": 0,
                            "width": 20,
                            "height": 20,
                            "dom": dom,
                        }
                    ],
                }
            ]
        }
        comparison = paired_corpus.compare_geometry_probes(state, state)
        rect_state = comparison[0]["rect_deltas"][0][
            "target_subtree_comparability"
        ]
        self.assertFalse(comparison[0]["geometry_verdict_valid"])
        self.assertEqual(
            rect_state["classification"], "insufficient-target-structure"
        )
        self.assertIn(
            "target-subtree-descriptor-truncated",
            comparison[0]["geometry_verdict_exclusion_reasons"],
        )

    def test_feature_comparison_reports_presence_computed_geometry_and_quad_capability(self):
        obscura = {
            "feature_probes": {
                "bounds": {"scan_limit": 2000},
                "scanned_elements": 12,
                "scan_truncated": False,
                "box_quads": {
                    "available": False,
                    "source": None,
                    "reason": "CLI has no node-scoped CDP",
                },
                "categories": {
                    "transform": {
                        "matches_seen": 1,
                        "candidates_truncated": False,
                        "candidates": [
                            {
                                "dom_index": 4,
                                "candidate_reasons": ["transform"],
                                "x": 12,
                                "y": 18,
                                "width": 30,
                                "height": 40,
                                "visible": True,
                                "computed": {
                                    "transform": "matrix(1, 0, 0, 1, 2, 0)"
                                },
                                "box_quads": None,
                            }
                        ],
                    }
                },
            }
        }
        chromium = {
            "feature_probes": {
                "bounds": {"scan_limit": 2000},
                "scanned_elements": 12,
                "scan_truncated": False,
                "box_quads": {
                    "available": True,
                    "source": "cdp.DOM.getBoxModel",
                },
                "categories": {
                    "transform": {
                        "matches_seen": 2,
                        "candidates_truncated": False,
                        "candidates": [
                            {
                                "dom_index": 4,
                                "candidate_reasons": ["transform"],
                                "x": 10,
                                "y": 20,
                                "width": 30,
                                "height": 39,
                                "visible": True,
                                "computed": {
                                    "transform": "matrix(1, 0, 0, 1, 0, 0)"
                                },
                                "box_quads": {
                                    "coordinate_space": "viewport-css-px",
                                    "content": [10, 20, 40, 20, 40, 59, 10, 59],
                                    "border": [10, 20, 40, 20, 40, 59, 10, 59],
                                },
                            },
                            {
                                "dom_index": 8,
                                "candidate_reasons": ["rotate"],
                                "computed": {"rotate": "10deg"},
                                "box_quads": None,
                            },
                        ],
                    }
                },
            }
        }
        comparison = paired_corpus.compare_feature_probes(obscura, chromium)
        category = comparison["categories"][0]
        self.assertEqual(category["kind"], "transform")
        self.assertEqual(category["matches_seen"]["delta"], -1)
        first = category["candidates"][0]
        self.assertEqual(
            first["geometry_delta"],
            {"x": 2, "y": -2, "width": 0, "height": 1},
        )
        self.assertEqual(first["computed_difference_count"], 1)
        self.assertFalse(first["box_quads"]["obscura_available"])
        self.assertTrue(first["box_quads"]["chromium_available"])
        self.assertIsNone(first["box_quads"]["content_delta"])
        second = category["candidates"][1]
        self.assertEqual(
            second["present"], {"obscura": False, "chromium": True}
        )


class AnimationSamplingTests(unittest.TestCase):
    class FakePage:
        def __init__(self):
            self.calls = []

        def evaluate(self, expression, *args):
            self.calls.append((expression, args))
            return {
                "supported": True,
                "requested_ms": args[0],
                "discovered": 3,
                "frozen": 3,
                "failures": [],
            }

    def test_explicit_sample_pauses_and_seeks_current_animations(self):
        page = self.FakePage()
        result = paired_corpus.freeze_chromium_animations(page, 0)
        self.assertEqual(result["requested_ms"], 0)
        self.assertEqual(len(page.calls), 1)
        expression, args = page.calls[0]
        self.assertEqual(args, (0,))
        self.assertIn("document.getAnimations()", expression)
        self.assertIn("animation.pause()", expression)
        self.assertIn("animation.currentTime = sampleMs", expression)
        self.assertIn("getBoundingClientRect", expression)


if __name__ == "__main__":
    unittest.main()
