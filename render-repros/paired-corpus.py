#!/usr/bin/env python3
"""Capture Obscura and Chromium concurrently with the same real settle delay.

Every run uses a new output directory, checks process status and non-empty
screenshots, records browser versions and timings, and reports raw full-canvas
pixel diagnostics plus background-tolerant structural-edge diagnostics from
check.py. Browser identity is pinned and both engines record DOM/text
fingerprints, viewport geometry, and JS-visible resource-readiness state at
capture time. Repeatable `--geometry-selector` probes can additionally retain
bounded, viewport-relative element rects from that same pre-screenshot sample.
It deliberately emits no aggregate parity verdict. An optional pre-change
Obscura binary can be captured concurrently so regressions are compared
against the same live-page moment.
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
from PIL import Image
from playwright.sync_api import sync_playwright

from check import pair_metrics


CANONICAL_USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/143.0.0.0 Safari/537.36"
)
CANONICAL_PLATFORM = "Win32"
CANONICAL_UA_PLATFORM = "Windows"
CANONICAL_UA_PLATFORM_VERSION = "10.0.0"
CANONICAL_OBSCURA_PROFILE = 0
CANONICAL_COLOR_SCHEME = "light"
# Obscura currently models the default motion preference, not `reduce`.
CANONICAL_REDUCED_MOTION = "no-preference"
EXPECTED_MEDIA_MATCHES = {
    "prefers_color_scheme_light": True,
    "prefers_color_scheme_dark": False,
    "prefers_reduced_motion_no_preference": True,
    "prefers_reduced_motion_reduce": False,
}
GREASE_CHARS = [" ", "(", ":", "-", ".", "/", ")", ";", "=", "?", "_"]
GREASE_VERSIONS = ["8", "99", "24"]
BRAND_PERMUTATIONS = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
]
GEOMETRY_PROBE_RECT_LIMIT = 200
GEOMETRY_PROBE_SUBTREE_LIMIT = 80
FEATURE_PROBE_SCAN_LIMIT = 2000
FEATURE_PROBE_CANDIDATE_LIMIT = 40
CAPTURE_BOUNDARY_PHASE = "capture-boundary-before-screenshot"
RESOURCE_WARMUP_PHASE = "before-final-scroll-reassert-and-state-sample"


def geometry_probe_javascript(selectors_expression):
    """Return bounded, per-selector geometry sampling JavaScript."""
    return (
        "const geometryClassName=element=>String("
        "element.getAttribute&&element.getAttribute('class')||'');"
        "const sampleGeometryDom=element=>{"
        "const descendants=element.querySelectorAll('*');const nodes=[element];"
        "for(let geometryIndex=0;geometryIndex<descendants.length&&nodes.length<"
        f"{GEOMETRY_PROBE_SUBTREE_LIMIT}"
        ";geometryIndex++)nodes.push(descendants[geometryIndex]);"
        "const nodeIndices=new Map(nodes.map((node,index)=>[node,index]));"
        "const describe=(node,index)=>({"
        "index:index,parent_index:index===0?null:"
        "(nodeIndices.has(node.parentElement)?nodeIndices.get(node.parentElement):null),"
        "tag:String(node.tagName||'').toLowerCase(),id:node.id||'',"
        "class_name:geometryClassName(node),"
        "child_element_count:node.children.length});"
        "return {tag:String(element.tagName||'').toLowerCase(),"
        "id:element.id||'',class_name:geometryClassName(element),"
        "child_node_count:element.childNodes.length,"
        "child_element_count:element.children.length,"
        "children:Array.from(element.children).slice(0,20).map(child=>({"
        "tag:String(child.tagName||'').toLowerCase(),id:child.id||'',"
        "class_name:geometryClassName(child)})),"
        "subtree_element_count:descendants.length+1,"
        f"subtree_limit:{GEOMETRY_PROBE_SUBTREE_LIMIT},"
        f"subtree_truncated:descendants.length+1>{GEOMETRY_PROBE_SUBTREE_LIMIT},"
        "subtree:nodes.map(describe)};};"
        "const sampleGeometrySelector=selector=>{try{"
        "const elements=Array.from(document.querySelectorAll(selector));"
        "const rects=elements.slice(0,"
        f"{GEOMETRY_PROBE_RECT_LIMIT}"
        ").map((element,index)=>{"
        "const rect=element.getBoundingClientRect();"
        "const style=getComputedStyle(element);"
        "const opacity=Number.parseFloat(style.opacity);"
        "const clientRectCount=element.getClientRects().length;"
        "return {index:index,x:rect.left,y:rect.top,"
        "width:rect.width,height:rect.height,"
        "dom:sampleGeometryDom(element),"
        "computed:{display:style.display,float:style.cssFloat,"
        "clear:style.clear,position:style.position,"
        "visibility:style.visibility,opacity:style.opacity,"
        "box_sizing:style.boxSizing,width:style.width,height:style.height,"
        "min_width:style.minWidth,min_height:style.minHeight,"
        "max_width:style.maxWidth,max_height:style.maxHeight,"
        "aspect_ratio:style.aspectRatio,object_fit:style.objectFit,"
        "object_position:style.objectPosition,"
        "margin_top:style.marginTop,margin_right:style.marginRight,"
        "margin_bottom:style.marginBottom,margin_left:style.marginLeft,"
        "padding_top:style.paddingTop,padding_right:style.paddingRight,"
        "padding_bottom:style.paddingBottom,padding_left:style.paddingLeft,"
        "border_top_width:style.borderTopWidth,"
        "border_right_width:style.borderRightWidth,"
        "border_bottom_width:style.borderBottomWidth,"
        "border_left_width:style.borderLeftWidth,"
        "border_top_style:style.borderTopStyle,"
        "border_right_style:style.borderRightStyle,"
        "border_bottom_style:style.borderBottomStyle,"
        "border_left_style:style.borderLeftStyle,"
        "border_top_color:style.borderTopColor,"
        "border_right_color:style.borderRightColor,"
        "border_bottom_color:style.borderBottomColor,"
        "border_left_color:style.borderLeftColor,"
        "border_top_left_radius:style.borderTopLeftRadius,"
        "border_top_right_radius:style.borderTopRightRadius,"
        "border_bottom_right_radius:style.borderBottomRightRadius,"
        "border_bottom_left_radius:style.borderBottomLeftRadius,"
        "font_family:style.fontFamily,font_size:style.fontSize,"
        "font_weight:style.fontWeight,line_height:style.lineHeight,"
        "letter_spacing:style.letterSpacing,white_space:style.whiteSpace,"
        "word_break:style.wordBreak,overflow_wrap:style.overflowWrap,"
        "text_overflow:style.textOverflow,"
        "webkit_line_clamp:style.webkitLineClamp,"
        "webkit_box_orient:style.webkitBoxOrient,"
        "direction:style.direction,unicode_bidi:style.unicodeBidi,"
        "flex_direction:style.flexDirection,flex_wrap:style.flexWrap,"
        "flex_grow:style.flexGrow,flex_shrink:style.flexShrink,"
        "flex_basis:style.flexBasis,align_self:style.alignSelf,"
        "align_items:style.alignItems,justify_content:style.justifyContent,"
        "justify_items:style.justifyItems,justify_self:style.justifySelf,"
        "grid_template_columns:style.gridTemplateColumns,"
        "grid_template_rows:style.gridTemplateRows,"
        "grid_column:style.gridColumn,grid_row:style.gridRow,"
        "column_gap:style.columnGap,row_gap:style.rowGap,"
        "text_align:style.textAlign,color:style.color,"
        "background_color:style.backgroundColor,"
        "background_image:style.backgroundImage,"
        "contain:style.contain,content_visibility:style.contentVisibility,"
        "transform:style.transform,"
        "transform_origin:style.transformOrigin,"
        "transform_box:style.transformBox,"
        "translate:style.translate,rotate:style.rotate,scale:style.scale,"
        "perspective:style.perspective,"
        "perspective_origin:style.perspectiveOrigin,"
        "filter:style.filter,"
        "backdrop_filter:style.backdropFilter,"
        "overflow_x:style.overflowX,overflow_y:style.overflowY},"
        "visible:clientRectCount>0&&rect.width>0&&rect.height>0"
        "&&style.display!=='none'&&style.visibility!=='hidden'"
        "&&style.visibility!=='collapse'"
        "&&(!Number.isFinite(opacity)||opacity>0),"
        "client_rect_count:clientRectCount};"
        "});"
        "return {selector:selector,valid:true,count:elements.length,"
        "coordinate_space:'viewport-css-px',"
        f"rect_limit:{GEOMETRY_PROBE_RECT_LIMIT},"
        f"rects_truncated:elements.length>{GEOMETRY_PROBE_RECT_LIMIT},"
        "rects:rects,error:null};"
        "}catch(error){return {selector:selector,valid:false,count:null,"
        "coordinate_space:'viewport-css-px',"
        f"rect_limit:{GEOMETRY_PROBE_RECT_LIMIT},"
        "rects_truncated:false,rects:[],"
        "error:{name:error&&error.name?String(error.name):'Error',"
        "message:error&&error.message?String(error.message):String(error)}};}};"
        f"const geometryProbes={selectors_expression}.map(sampleGeometrySelector);"
    )


def feature_probe_javascript(box_quad_unavailable_reason):
    """Return bounded selector-free transform/truncation discovery JavaScript."""
    return (
        "const featureProbeElements=document.getElementsByTagName('*');"
        "const featureProbeScanned=Math.min(featureProbeElements.length,"
        f"{FEATURE_PROBE_SCAN_LIMIT}"
        ");"
        "const featureProbeCategories={"
        "transform:{matches_seen:0,candidates:[]},"
        "text_truncation:{matches_seen:0,candidates:[]}};"
        "const featureProbeCandidate=(kind,element,domIndex,comparisonIndex,"
        "style,reasons)=>{"
        "const rect=element.getBoundingClientRect();"
        "const opacity=Number.parseFloat(style.opacity);"
        "const clientRectCount=element.getClientRects().length;"
        "const common={display:style.display,position:style.position,"
        "visibility:style.visibility,opacity:style.opacity,"
        "overflow_x:style.overflowX,overflow_y:style.overflowY};"
        "const computed=kind==='transform'?Object.assign(common,{"
        "transform:style.transform,transform_origin:style.transformOrigin,"
        "transform_box:style.transformBox,translate:style.translate,"
        "rotate:style.rotate,scale:style.scale,"
        "perspective:style.perspective,"
        "perspective_origin:style.perspectiveOrigin}):Object.assign(common,{"
        "white_space:style.whiteSpace,text_overflow:style.textOverflow,"
        "webkit_line_clamp:style.webkitLineClamp,"
        "webkit_box_orient:style.webkitBoxOrient,"
        "line_height:style.lineHeight,height:style.height,"
        "max_height:style.maxHeight});"
        "return {kind:kind,dom_index:domIndex,"
        "comparison_index:comparisonIndex,candidate_reasons:reasons,"
        "dom:{tag:String(element.tagName||'').toLowerCase(),"
        "id:element.id||'',class_name:typeof element.className==='string'"
        "?element.className:''},x:rect.left,y:rect.top,width:rect.width,"
        "height:rect.height,visible:clientRectCount>0&&rect.width>0"
        "&&rect.height>0&&style.display!=='none'"
        "&&style.visibility!=='hidden'&&style.visibility!=='collapse'"
        "&&(!Number.isFinite(opacity)||opacity>0),"
        "client_rect_count:clientRectCount,computed:computed,box_quads:null};};"
        "let featureProbeComparisonIndex=0;"
        "for(let domIndex=0;domIndex<featureProbeScanned;domIndex++){"
        "const element=featureProbeElements[domIndex];"
        "const obscuraStyleMirror=element.tagName==='STYLE'&&"
        "(element.hasAttribute('data-obscura-external-stylesheets')||"
        "element.hasAttribute('data-obscura-linked'));"
        "if(obscuraStyleMirror)continue;"
        "const comparisonIndex=featureProbeComparisonIndex++;"
        "const style=getComputedStyle(element);"
        "const transformReasons=[];"
        "if(style.transform&&style.transform!=='none')"
        "transformReasons.push('transform');"
        "if(style.translate&&style.translate!=='none')"
        "transformReasons.push('translate');"
        "if(style.rotate&&style.rotate!=='none')"
        "transformReasons.push('rotate');"
        "if(style.scale&&style.scale!=='none')"
        "transformReasons.push('scale');"
        "if(style.perspective&&style.perspective!=='none')"
        "transformReasons.push('perspective');"
        "if(transformReasons.length){"
        "const category=featureProbeCategories.transform;"
        "category.matches_seen++;"
        f"if(category.candidates.length<{FEATURE_PROBE_CANDIDATE_LIMIT})"
        "category.candidates.push(featureProbeCandidate("
        "'transform',element,domIndex,comparisonIndex,style,transformReasons));}"
        "const truncationReasons=[];"
        "if(style.textOverflow&&style.textOverflow!=='clip')"
        "truncationReasons.push('text-overflow');"
        "if(style.webkitLineClamp&&style.webkitLineClamp!=='none')"
        "truncationReasons.push('-webkit-line-clamp');"
        "if(truncationReasons.length){"
        "const category=featureProbeCategories.text_truncation;"
        "category.matches_seen++;"
        f"if(category.candidates.length<{FEATURE_PROBE_CANDIDATE_LIMIT})"
        "category.candidates.push(featureProbeCandidate("
        "'text_truncation',element,domIndex,comparisonIndex,style,"
        "truncationReasons));}}"
        "for(const category of Object.values(featureProbeCategories)){"
        "category.candidates_truncated=category.matches_seen>"
        f"{FEATURE_PROBE_CANDIDATE_LIMIT}"
        ";}"
        "const featureProbes={version:1,bounds:{scan_limit:"
        f"{FEATURE_PROBE_SCAN_LIMIT},candidate_limit_per_category:"
        f"{FEATURE_PROBE_CANDIDATE_LIMIT}"
        "},scanned_elements:featureProbeScanned,"
        "comparable_scanned_elements:featureProbeComparisonIndex,"
        "scan_truncated:featureProbeElements.length>featureProbeScanned,"
        "total_elements:featureProbeElements.length,"
        "categories:featureProbeCategories,box_quads:{available:false,"
        "source:null,coordinate_space:null,attempted_candidates:0,"
        "captured_candidates:0,failures:[],reason:"
        f"{json.dumps(box_quad_unavailable_reason)}"
        "}};"
    )


def slug(url):
    value = re.sub(r"[^a-z0-9]+", "-", url.lower()).strip("-")
    return value[:80] or "page"


def binary_version(binary):
    result = subprocess.run(
        [binary, "--version"], capture_output=True, text=True, timeout=10
    )
    text = (result.stdout or result.stderr).strip()
    return {"status": result.returncode, "text": text}


def diagnostic_text(value):
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return value


def media_matches_configured(media):
    return all(media.get(key) is expected for key, expected in EXPECTED_MEDIA_MATCHES.items())


def parse_scroll_y(value):
    if value == "bottom":
        return value
    try:
        return int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            "--scroll-y must be an integer CSS-pixel offset or 'bottom'"
        ) from error


def scroll_eval_expression(scroll):
    """Compatibility wrapper used by older callers and focused tests."""
    return obscura_state_eval_expression(scroll)


def reassert_chromium_controlled_scroll(page, scroll):
    """Reassert an exact capture offset after the page's settle interval.

    The first controlled scroll intentionally obeys the page's authored
    `scroll-behavior` and participates in ordinary scroll anchoring while the
    page settles. That is useful runtime evidence, but it is not a stable
    screenshot coordinate: a shrinking region above the viewport can move the
    final offset even when both engines laid it out correctly. Preserve that
    settled offset, then issue one explicit instant scroll immediately before
    state sampling and paint.
    """
    scroll_x, scroll_y = scroll
    return page.evaluate(
        """([x, y]) => {
          const requestedY = y === "bottom"
            ? document.documentElement.scrollHeight
            : y;
          const beforeReassert = {x: window.scrollX, y: window.scrollY};
          try {
            window.scrollTo({
              left: x,
              top: requestedY,
              behavior: "instant"
            });
          } catch (_) {
            const root = document.documentElement;
            const previous = root ? root.style.getPropertyValue(
              "scroll-behavior") : "";
            const priority = root ? root.style.getPropertyPriority(
              "scroll-behavior") : "";
            if (root) {
              root.style.setProperty("scroll-behavior", "auto", "important");
            }
            window.scrollTo(x, requestedY);
            if (root) {
              if (previous) {
                root.style.setProperty("scroll-behavior", previous, priority);
              } else {
                root.style.removeProperty("scroll-behavior");
              }
            }
          }
          return {
            requested: {x, y: requestedY},
            pre_reassert_actual: beforeReassert,
            final_actual: {x: window.scrollX, y: window.scrollY},
            reassert_behavior: "instant",
            pre_reassert_phase: "after-controlled-scroll-settle",
            final_phase: "immediately-before-state-and-screenshot"
          };
        }""",
        [scroll_x, scroll_y],
    )


def obscura_state_eval_expression(
    scroll,
    geometry_selectors=None,
    sampled_phase="before-cli-post-eval-settle",
):
    """Build a page-state sample with an explicit phase label.

    `scroll` remains for compatibility with focused tests and ordinary callers.
    Paired captures perform their authored scroll before the second settle in
    the CLI, then invoke this expression without scrolling at the final capture
    boundary.
    """
    scroll_script = ""
    requested = "null"
    controlled_scroll_result = "null"
    if scroll is not None:
        scroll_x, scroll_y = scroll
        requested_y = (
            "document.documentElement.scrollHeight"
            if scroll_y == "bottom"
            else str(scroll_y)
        )
        scroll_script = (
            f"const requestedX={scroll_x},requestedY={requested_y};"
            "const preInitialActual={x:window.scrollX,y:window.scrollY};"
            "window.scrollTo(requestedX,requestedY);"
            "const postInitialActual={x:window.scrollX,y:window.scrollY};"
        )
        requested = "{x:requestedX,y:requestedY}"
        controlled_scroll_result = (
            "{requested:{x:requestedX,y:requestedY},"
            "pre_initial_actual:preInitialActual,"
            "post_initial_actual:postInitialActual,"
            "initial_behavior:'authored',"
            "initial_phase:'before-cli-post-eval-settle',"
            "final_phase:'cli-capture-state-after-post-eval-settle'}"
        )
    geometry_setup = ""
    geometry_result = ""
    if geometry_selectors:
        selectors_json = json.dumps(
            list(geometry_selectors), ensure_ascii=True, separators=(",", ":")
        )
        geometry_setup = geometry_probe_javascript(selectors_json)
        geometry_result = "geometry_probes:geometryProbes,"
    return (
        "(()=>{"
        + scroll_script
        + "const root=document.documentElement,body=document.body;"
        "const dom=root?root.outerHTML:'';"
        "const injectedStyles=root?Array.from(root.querySelectorAll("
        "'style[data-obscura-external-stylesheets],style[data-obscura-linked]'"
        ")):[];"
        "const normalizedDom=injectedStyles.reduce((html,node)=>"
        "typeof node.outerHTML==='string'?html.replace(node.outerHTML,''):html,dom);"
        "const bodyText=body?(body.textContent||'').replace(/\\s+/g,' ').trim():'';"
        "const images=Array.from(document.images||[]),fonts=document.fonts;"
        "const hash=value=>{let h=2166136261;"
        "for(let i=0;i<value.length;i++){h^=value.charCodeAt(i);"
        "h=Math.imul(h,16777619)}"
        "return ('00000000'+(h>>>0).toString(16)).slice(-8)};"
        + feature_probe_javascript(
            "Obscura CLI capture does not expose node-scoped CDP "
            "DOM.getBoxModel at the screenshot boundary"
        )
        + geometry_setup
        + "return JSON.stringify({"
        f"sampled_phase:{json.dumps(sampled_phase)},"
        "feature_probes:featureProbes,"
        + geometry_result
        + f"requested:{requested},"
        + f"controlled_scroll:{controlled_scroll_result},"
        "url:location.href,"
        "document:{ready_state:document.readyState,"
        "element_count:document.getElementsByTagName('*').length,"
        "outer_html_utf16:dom.length,outer_html_fnv1a32:hash(dom),"
        "normalized_outer_html_utf16:normalizedDom.length,"
        "normalized_outer_html_fnv1a32:hash(normalizedDom),"
        "body_text_utf16:bodyText.length,body_text_fnv1a32:hash(bodyText)},"
        "geometry:{inner_width:innerWidth,inner_height:innerHeight,"
        "scroll_x:scrollX,scroll_y:scrollY,"
        "document_client_width:root?root.clientWidth:null,"
        "document_client_height:root?root.clientHeight:null,"
        "document_scroll_width:root?root.scrollWidth:null,"
        "document_scroll_height:root?root.scrollHeight:null,"
        "body_client_width:body?body.clientWidth:null,"
        "body_client_height:body?body.clientHeight:null,"
        "body_scroll_width:body?body.scrollWidth:null,"
        "body_scroll_height:body?body.scrollHeight:null},"
        "fonts:{supported:!!fonts,status:fonts?fonts.status:null,"
        "face_count:fonts?Array.from(fonts).length:null,"
        "ready_at_sample:fonts?fonts.status==='loaded':null},"
        "images:{total:images.length,"
        "complete:images.filter(image=>image.complete).length,"
        "complete_with_pixels:images.filter(image=>image.complete&&image.naturalWidth>0).length,"
        "complete_without_pixels:images.filter(image=>image.complete&&image.naturalWidth===0).length,"
        "pending:images.filter(image=>!image.complete).length,"
        "lazy:images.filter(image=>image.loading==='lazy').length},"
        "media:{"
        "prefers_color_scheme_light:matchMedia('(prefers-color-scheme: light)').matches,"
        "prefers_color_scheme_dark:matchMedia('(prefers-color-scheme: dark)').matches,"
        "prefers_reduced_motion_no_preference:matchMedia('(prefers-reduced-motion: no-preference)').matches,"
        "prefers_reduced_motion_reduce:matchMedia('(prefers-reduced-motion: reduce)').matches}"
        "})"
        "})()"
    )


def parse_obscura_capture_report(stdout):
    """Parse the CLI's evaluation plus authoritative screenshot capture state."""
    for line in reversed(stdout.splitlines()):
        try:
            report = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(report, dict) or not isinstance(
            report.get("captureState"), dict
        ):
            continue
        evaluated = report.get("evaluation")
        if isinstance(evaluated, str):
            try:
                evaluated = json.loads(evaluated)
            except json.JSONDecodeError:
                evaluated = None
        if not isinstance(evaluated, dict):
            continue
        state = dict(evaluated)
        geometry = dict(state.get("geometry") or {})
        capture = report["captureState"]
        # These values come from the exact prepared render used by screenshot,
        # so they take precedence over the JS sample if the two ever diverge.
        geometry.update(
            {
                "inner_width": capture.get("innerWidth"),
                "inner_height": capture.get("innerHeight"),
                "scroll_x": capture.get("scrollX"),
                "scroll_y": capture.get("scrollY"),
                "document_scroll_width": capture.get("scrollWidth"),
                "document_scroll_height": capture.get("scrollHeight"),
            }
        )
        state["geometry"] = geometry
        if isinstance(report.get("controlledScroll"), dict):
            state["controlled_scroll_capture"] = report["controlledScroll"]
        if isinstance(report.get("resourceWarmup"), dict):
            state["resource_warmup"] = report["resourceWarmup"]
        state["evaluation_sampled_phase"] = state.get("sampled_phase")
        state["screenshot_sampled_phase"] = CAPTURE_BOUNDARY_PHASE
        state["state_and_screenshot_share_capture_boundary"] = (
            state.get("sampled_phase") == CAPTURE_BOUNDARY_PHASE
        )
        return state
    return None


def parse_obscura_scroll_report(stdout):
    state = parse_obscura_capture_report(stdout)
    if state is None:
        return None
    geometry = state.get("geometry") or {}
    controlled = state.get("controlled_scroll") or {}
    capture_controlled = state.get("controlled_scroll_capture") or {}
    return {
        "requested": (
            capture_controlled.get("requested")
            or controlled.get("requested")
            or state.get("requested")
        ),
        "pre_reassert_actual": capture_controlled.get("preReassertActual"),
        "pre_initial_actual": (
            capture_controlled.get("preInitialActual")
            or controlled.get("pre_initial_actual")
        ),
        "post_initial_actual": (
            capture_controlled.get("postInitialActual")
            or controlled.get("post_initial_actual")
        ),
        "final_reassert_actual": capture_controlled.get(
            "finalReassertActual"
        ),
        "actual": {"x": geometry.get("scroll_x"), "y": geometry.get("scroll_y")},
        "viewport": {
            "width": geometry.get("inner_width"),
            "height": geometry.get("inner_height"),
        },
        "content": {
            "width": geometry.get("document_scroll_width"),
            "height": geometry.get("document_scroll_height"),
        },
        "reassert_behavior": capture_controlled.get("behavior"),
        "pre_reassert_phase": capture_controlled.get("phase"),
        "final_phase": (
            capture_controlled.get("phase")
            or controlled.get("final_phase")
        ),
        "sampled_phase": state["sampled_phase"],
    }


def obscura_environment(width, height, animation_time_ms=None):
    env = dict(
        os.environ,
        OBSCURA_SHOT_W=str(width),
        OBSCURA_SHOT_H=str(height),
        OBSCURA_ALLOW_PRIVATE_NETWORK="1",
        # Product fetches return early when the event loop becomes idle. Paired
        # captures instead need the same complete post-load wall interval that
        # Playwright's wait_for_timeout below uses.
        OBSCURA_STRICT_SETTLE="1",
        # Match Playwright's 50-second goto allowance. This is the browser
        # engine's millisecond ceiling, distinct from the CLI's seconds unit.
        OBSCURA_NAV_TIMEOUT_MS="50000",
        # Pin the navigator platform/profile as well as the explicit UA. A
        # randomized platform changes responsive content and font selection,
        # making a renderer comparison answer the wrong question.
        OBSCURA_PROFILE=str(CANONICAL_OBSCURA_PROFILE),
        # Run the paired corpus's read-only state/selector evaluation only after
        # all settle phases and the final scroll reassertion. No event-loop
        # pumping occurs between that evaluation and screenshot paint.
        OBSCURA_SHOT_EVAL_AT_CAPTURE="1",
        # Resolve the exact retained image/font graph with one throwaway paint
        # before resource-readiness state is sampled. Chromium performs the
        # same warm-up shot in the paired process.
        OBSCURA_SHOT_RESOURCE_WARMUP="1",
    )
    if animation_time_ms is not None:
        env["OBSCURA_SHOT_ANIMATION_TIME_MS"] = str(animation_time_ms)
    return env


def with_controlled_scroll_environment(env, scroll):
    """Copy capture-only final scroll coordinates into the CLI environment."""
    if scroll is not None:
        env["OBSCURA_SHOT_SCROLL_X"] = str(scroll[0])
        env["OBSCURA_SHOT_SCROLL_Y"] = str(scroll[1])
    return env


def probe_obscura_identity(binary):
    expression = (
        "JSON.stringify({userAgent:navigator.userAgent,"
        "platform:navigator.platform,"
        "uaPlatform:navigator.userAgentData&&navigator.userAgentData.platform,"
        "uaBrands:navigator.userAgentData&&navigator.userAgentData.brands,"
        "media:{"
        "prefers_color_scheme_light:matchMedia('(prefers-color-scheme: light)').matches,"
        "prefers_color_scheme_dark:matchMedia('(prefers-color-scheme: dark)').matches,"
        "prefers_reduced_motion_no_preference:matchMedia('(prefers-reduced-motion: no-preference)').matches,"
        "prefers_reduced_motion_reduce:matchMedia('(prefers-reduced-motion: reduce)').matches"
        "}})"
    )
    command = [
        binary,
        "fetch",
        "data:text/html,<title>identity-probe</title>",
        "--user-agent",
        CANONICAL_USER_AGENT,
        "--eval",
        expression,
        "--timeout",
        "5",
        "--wait",
        "0",
        "--quiet",
    ]
    env = obscura_environment(1, 1)
    try:
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=15, env=env
        )
        raw = result.stdout.strip()
        effective = json.loads(raw) if result.returncode == 0 else None
        return {
            "ok": result.returncode == 0,
            "status": result.returncode,
            "effective": effective,
            "media_matches_configured": (
                media_matches_configured(effective.get("media", {}))
                if effective
                else False
            ),
            "diagnostic": result.stderr.strip() or None,
        }
    except (subprocess.TimeoutExpired, json.JSONDecodeError) as error:
        return {"ok": False, "status": "probe-error", "diagnostic": str(error)}


def probe_obscura_css_media(binary):
    """Verify the renderer's CSS media evaluator, not only JS matchMedia."""
    html = """<!doctype html><style>
      html,body{margin:0}
      #scheme,#motion{position:fixed;top:0;width:4px;height:4px;background:#ff00ff}
      #scheme{left:0} #motion{left:4px}
      @media (prefers-color-scheme:light){#scheme{background:#00ff00}}
      @media (prefers-color-scheme:dark){#scheme{background:#ff0000}}
      @media (prefers-reduced-motion:no-preference){#motion{background:#0000ff}}
      @media (prefers-reduced-motion:reduce){#motion{background:#ffff00}}
      </style><div id=scheme></div><div id=motion></div>"""
    url = "data:text/html," + urllib.parse.quote(html, safe="")
    with tempfile.TemporaryDirectory(prefix="obscura-media-probe-") as directory:
        screenshot = Path(directory) / "probe.png"
        command = [
            binary,
            "fetch",
            url,
            "--user-agent",
            CANONICAL_USER_AGENT,
            "--screenshot",
            str(screenshot),
            "--timeout",
            "5",
            "--wait",
            "0",
            "--quiet",
        ]
        try:
            result = subprocess.run(
                command,
                capture_output=True,
                text=True,
                timeout=15,
                env=obscura_environment(8, 4),
            )
            if result.returncode != 0 or not screenshot.is_file():
                return {
                    "ok": False,
                    "status": result.returncode,
                    "diagnostic": result.stderr.strip() or "missing probe screenshot",
                }
            image = Image.open(screenshot).convert("RGB")
            scheme = list(image.getpixel((1, 1)))
            motion = list(image.getpixel((5, 1)))
            expected = {"color_scheme": [0, 255, 0], "reduced_motion": [0, 0, 255]}
            actual = {"color_scheme": scheme, "reduced_motion": motion}
            return {
                "ok": actual == expected,
                "status": result.returncode,
                "configured": {
                    "color_scheme": CANONICAL_COLOR_SCHEME,
                    "reduced_motion": CANONICAL_REDUCED_MOTION,
                },
                "expected_rgb": expected,
                "actual_rgb": actual,
                "diagnostic": None if actual == expected else "CSS media probe colors differ",
            }
        except (subprocess.TimeoutExpired, OSError) as error:
            return {"ok": False, "status": "probe-error", "diagnostic": str(error)}


def capture_obscura(
    binary,
    url,
    screenshot,
    log,
    width,
    height,
    settle_ms,
    scroll=None,
    geometry_selectors=None,
    animation_time_ms=None,
):
    env = with_controlled_scroll_environment(
        obscura_environment(width, height, animation_time_ms), scroll
    )
    command = [
        binary,
        "fetch",
        url,
        "--user-agent",
        CANONICAL_USER_AGENT,
        "--screenshot",
        str(screenshot),
        "--timeout",
        "50",
        "--wait",
        f"{settle_ms / 1000:g}",
    ]
    state_expression = obscura_state_eval_expression(
        None,
        geometry_selectors,
        sampled_phase=CAPTURE_BOUNDARY_PHASE,
    )
    command.extend(["--eval", state_expression])
    started = time.time()
    try:
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=75, env=env
        )
        log.write_text(result.stdout + result.stderr)
        state = (
            parse_obscura_capture_report(result.stdout)
            if result.returncode == 0
            else None
        )
        if state is not None:
            media = state.get("media") or {}
            media["matches_configured"] = media_matches_configured(media)
            state["media"] = media
        scroll_state = (
            parse_obscura_scroll_report(result.stdout)
            if scroll is not None and state is not None
            else None
        )
        ok = (
            result.returncode == 0
            and screenshot.is_file()
            and screenshot.stat().st_size > 0
            and state is not None
            and state["media"]["matches_configured"]
            and (scroll is None or scroll_state is not None)
        )
        return {
            "ok": ok,
            "status": result.returncode,
            "elapsed_s": round(time.time() - started, 3),
            "state": state,
            "scroll_state": scroll_state,
        }
    except subprocess.TimeoutExpired as error:
        log.write_text(
            diagnostic_text(error.stdout) + diagnostic_text(error.stderr)
        )
        return {"ok": False, "status": "timeout", "elapsed_s": round(time.time() - started, 3)}


def chromium_identity_override(session):
    """Keep request headers and navigator identity aligned with Obscura."""
    match = re.search(r"Chrome/(\d+)", CANONICAL_USER_AGENT)
    major = int(match.group(1)) if match else 143
    grease = {
        "brand": (
            "Not"
            + GREASE_CHARS[major % len(GREASE_CHARS)]
            + "A"
            + GREASE_CHARS[(major + 1) % len(GREASE_CHARS)]
            + "Brand"
        ),
        "version": GREASE_VERSIONS[major % len(GREASE_VERSIONS)],
    }
    unordered = [
        grease,
        {"brand": "Chromium", "version": str(major)},
        {"brand": "Google Chrome", "version": str(major)},
    ]
    permutation = BRAND_PERMUTATIONS[major % len(BRAND_PERMUTATIONS)]
    brands = [unordered[index] for index in permutation]
    session.send(
        "Emulation.setUserAgentOverride",
        {
            "userAgent": CANONICAL_USER_AGENT,
            "acceptLanguage": "en-US,en",
            "platform": CANONICAL_PLATFORM,
            "userAgentMetadata": {
                "brands": brands,
                "fullVersionList": [
                    {
                        "brand": brand["brand"],
                        "version": brand["version"] + ".0.0.0",
                    }
                    for brand in brands
                ],
                "platform": CANONICAL_UA_PLATFORM,
                "platformVersion": CANONICAL_UA_PLATFORM_VERSION,
                "architecture": "x86",
                "model": "",
                "mobile": False,
                "bitness": "64",
                "wow64": False,
            },
        },
    )


def capture_chromium_state(page, geometry_selectors=None):
    """Synchronously sample page provenance immediately before screenshot.

    Keep the in-page evaluation free of promises and event-loop yields. Hashing
    is intentionally performed on the host after the synchronous snapshot
    returns; awaiting WebCrypto here would let page tasks mutate layout between
    the geometry sample and screenshot paint while falsely labeling both as one
    capture boundary.
    """
    expression = """() => {
          function fnv1a32(value) {
            let hash = 2166136261;
            for (let index = 0; index < value.length; index++) {
              hash ^= value.charCodeAt(index);
              hash = Math.imul(hash, 16777619);
            }
            return (hash >>> 0).toString(16).padStart(8, "0");
          }

          const root = document.documentElement;
          const body = document.body;
          const dom = root ? root.outerHTML : "";
          const injectedStyles = root ? Array.from(root.querySelectorAll(
            "style[data-obscura-external-stylesheets],style[data-obscura-linked]"
          )) : [];
          const normalizedDom = injectedStyles.reduce(
            (html, node) => typeof node.outerHTML === "string"
              ? html.replace(node.outerHTML, "")
              : html,
            dom
          );
          const bodyText = body ? (body.textContent || "") : "";
          const normalizedBodyText = bodyText.replace(/\\s+/g, " ").trim();
          const images = Array.from(document.images || []);
          const fonts = document.fonts;
          const media = {
            prefers_color_scheme_light:
              matchMedia("(prefers-color-scheme: light)").matches,
            prefers_color_scheme_dark:
              matchMedia("(prefers-color-scheme: dark)").matches,
            prefers_reduced_motion_no_preference:
              matchMedia("(prefers-reduced-motion: no-preference)").matches,
            prefers_reduced_motion_reduce:
              matchMedia("(prefers-reduced-motion: reduce)").matches
          };
          return {
            _hash_sources: {
              dom,
              normalized_dom: normalizedDom,
              body_text: normalizedBodyText
            },
            sampled_phase: __CAPTURE_BOUNDARY_PHASE__,
            url: location.href,
            identity: {
              user_agent: navigator.userAgent,
              platform: navigator.platform,
              ua_platform: navigator.userAgentData
                ? navigator.userAgentData.platform
                : null,
              ua_brands: navigator.userAgentData
                ? Array.from(navigator.userAgentData.brands)
                : null,
              language: navigator.language,
              languages: Array.from(navigator.languages || []),
            },
            document: {
              ready_state: document.readyState,
              element_count: document.getElementsByTagName("*").length,
              outer_html_utf16: dom.length,
              outer_html_fnv1a32: fnv1a32(dom),
              outer_html_bytes: new TextEncoder().encode(dom).length,
              normalized_outer_html_utf16: normalizedDom.length,
              normalized_outer_html_fnv1a32: fnv1a32(normalizedDom),
              normalized_outer_html_bytes:
                new TextEncoder().encode(normalizedDom).length,
              body_text_utf16: normalizedBodyText.length,
              body_text_fnv1a32: fnv1a32(normalizedBodyText),
              body_text_bytes: new TextEncoder().encode(normalizedBodyText).length,
            },
            geometry: {
              inner_width: innerWidth,
              inner_height: innerHeight,
              scroll_x: scrollX,
              scroll_y: scrollY,
              device_pixel_ratio: devicePixelRatio,
              document_client_width: root ? root.clientWidth : null,
              document_client_height: root ? root.clientHeight : null,
              document_scroll_width: root ? root.scrollWidth : null,
              document_scroll_height: root ? root.scrollHeight : null,
              body_client_width: body ? body.clientWidth : null,
              body_client_height: body ? body.clientHeight : null,
              body_scroll_width: body ? body.scrollWidth : null,
              body_scroll_height: body ? body.scrollHeight : null,
              visual_viewport: visualViewport ? {
                width: visualViewport.width,
                height: visualViewport.height,
                scale: visualViewport.scale,
                offset_left: visualViewport.offsetLeft,
                offset_top: visualViewport.offsetTop
              } : null
            },
            fonts: {
              supported: !!fonts,
              status: fonts ? fonts.status : null,
              face_count: fonts ? Array.from(fonts).length : null,
              ready_at_sample: fonts ? fonts.status === "loaded" : null
            },
            images: {
              total: images.length,
              complete: images.filter(image => image.complete).length,
              complete_with_pixels: images.filter(image =>
                image.complete && image.naturalWidth > 0).length,
              complete_without_pixels: images.filter(image =>
                image.complete && image.naturalWidth === 0).length,
              pending: images.filter(image => !image.complete).length,
              lazy: images.filter(image => image.loading === "lazy").length
            },
            media: {
              ...media,
              root_computed_color_scheme: root
                ? getComputedStyle(root).colorScheme
                : null,
              root_class: root ? root.className : null,
              root_data_theme: root ? root.getAttribute("data-theme") : null,
              body_class: body ? body.className : null,
              body_data_theme: body ? body.getAttribute("data-theme") : null
            }
          };
        }"""
    expression = expression.replace(
        "__CAPTURE_BOUNDARY_PHASE__", json.dumps(CAPTURE_BOUNDARY_PHASE), 1
    )
    expression = expression.replace(
        "          const root = document.documentElement;",
        feature_probe_javascript(
            "Chromium CDP box quads have not been attached by the capture adapter"
        )
        + "          const root = document.documentElement;",
        1,
    )
    expression = expression.replace(
        "          return {\n            _hash_sources:",
        "          return {\n            feature_probes: featureProbes,\n"
        "            _hash_sources:",
        1,
    )
    if geometry_selectors:
        expression = expression.replace(
            "() => {", "geometrySelectors => {", 1
        )
        expression = expression.replace(
            "          const root = document.documentElement;",
            geometry_probe_javascript("geometrySelectors")
            + "          const root = document.documentElement;",
            1,
        )
        expression = expression.replace(
            "          return {\n            feature_probes: featureProbes,\n"
            "            _hash_sources:",
            '          return {\n            geometry_probes: geometryProbes,\n'
            "            feature_probes: featureProbes,\n"
            "            _hash_sources:",
            1,
        )
        state = page.evaluate(expression, list(geometry_selectors))
    else:
        state = page.evaluate(expression)
    return state


def finalize_chromium_state_hashes(state):
    """Finalize hashes after paint without touching the live Chromium page."""
    document_state = state["document"]
    hash_sources = state.pop("_hash_sources", None)
    if hash_sources is not None:
        document_state["outer_html_sha256"] = hashlib.sha256(
            hash_sources["dom"].encode()
        ).hexdigest()
        document_state["normalized_outer_html_sha256"] = hashlib.sha256(
            hash_sources["normalized_dom"].encode()
        ).hexdigest()
        document_state["body_text_sha256"] = hashlib.sha256(
            hash_sources["body_text"].encode()
        ).hexdigest()
    return state


def attach_chromium_box_quads(session, state):
    """Attach truthful CDP box-model quads to selector-free candidates.

    DOM.getBoxModel reports viewport-space quads after transforms. Keep these
    distinct from getBoundingClientRect's viewport-space axis-aligned box.
    """
    feature_probes = state.get("feature_probes") or {}
    categories = feature_probes.get("categories") or {}
    candidates = [
        candidate
        for category in categories.values()
        for candidate in (category.get("candidates") or [])
    ]
    unique_indices = sorted(
        {
            candidate.get("dom_index")
            for candidate in candidates
            if isinstance(candidate.get("dom_index"), int)
        }
    )
    capability = {
        "available": True,
        "source": "cdp.DOM.getBoxModel",
        "coordinate_space": "viewport-css-px",
        "attempted_candidates": len(unique_indices),
        "captured_candidates": 0,
        "failures": [],
        "reason": None,
    }
    models = {}
    object_group = "obscura-parity-feature-probes"
    try:
        for dom_index in unique_indices:
            try:
                remote = session.send(
                    "Runtime.evaluate",
                    {
                        "expression": (
                            "document.getElementsByTagName('*')["
                            f"{dom_index}]"
                        ),
                        "objectGroup": object_group,
                        "returnByValue": False,
                        "silent": True,
                    },
                ).get("result") or {}
                object_id = remote.get("objectId")
                if not object_id:
                    raise RuntimeError("candidate did not resolve to a remote object")
                model = session.send(
                    "DOM.getBoxModel", {"objectId": object_id}
                ).get("model") or {}
                content = model.get("content")
                border = model.get("border")
                if not (
                    isinstance(content, list)
                    and len(content) == 8
                    and isinstance(border, list)
                    and len(border) == 8
                ):
                    raise RuntimeError("CDP returned an incomplete box model")
                models[dom_index] = {
                    "source": "cdp.DOM.getBoxModel",
                    "coordinate_space": "viewport-css-px",
                    "content": content,
                    "border": border,
                }
                capability["captured_candidates"] += 1
            except Exception as error:
                capability["failures"].append(
                    {
                        "dom_index": dom_index,
                        "name": type(error).__name__,
                        "message": str(error),
                    }
                )
    finally:
        try:
            session.send("Runtime.releaseObjectGroup", {"objectGroup": object_group})
        except Exception:
            pass
    for candidate in candidates:
        candidate["box_quads"] = models.get(candidate.get("dom_index"))
    feature_probes["box_quads"] = capability
    state["feature_probes"] = feature_probes
    return state


def mark_chromium_box_quads_unavailable(state):
    """Record an absent CDP adapter explicitly; never synthesize AABB quads."""
    feature_probes = state.get("feature_probes") or {}
    feature_probes["box_quads"] = {
        "available": False,
        "source": None,
        "coordinate_space": None,
        "attempted_candidates": 0,
        "captured_candidates": 0,
        "failures": [],
        "reason": "Chromium capture did not receive a CDP session",
    }
    state["feature_probes"] = feature_probes
    return state


def prepare_chromium_feature_probes(state, cdp_session):
    if cdp_session is None:
        return mark_chromium_box_quads_unavailable(state)
    return attach_chromium_box_quads(cdp_session, state)


def chromium_capture_signature(state):
    """Return the capture-critical state used to bracket a Chromium PNG.

    Full DOM hashes remain useful provenance but are intentionally excluded:
    analytics nodes can churn without affecting paint. The signature focuses
    on viewport/content geometry, resource readiness, and every requested
    geometry probe.
    """
    document = state.get("document") or {}
    geometry = state.get("geometry") or {}
    fonts = state.get("fonts") or {}
    images = state.get("images") or {}
    probes = []
    for probe in state.get("geometry_probes") or []:
        probes.append(
            {
                "selector": probe.get("selector"),
                "valid": probe.get("valid"),
                "count": probe.get("count"),
                "rects_truncated": probe.get("rects_truncated"),
                "rects": [
                    {
                        key: rect.get(key)
                        for key in (
                            "x",
                            "y",
                            "width",
                            "height",
                            "visible",
                            "client_rect_count",
                            "dom",
                        )
                    }
                    for rect in probe.get("rects") or []
                ],
            }
        )
    feature_probes = state.get("feature_probes") or {}
    feature_categories = {}
    for kind, category in sorted((feature_probes.get("categories") or {}).items()):
        feature_categories[kind] = {
            "matches_seen": category.get("matches_seen"),
            "candidates_truncated": category.get("candidates_truncated"),
            "candidates": [
                {
                    key: candidate.get(key)
                    for key in (
                        "dom_index",
                        "comparison_index",
                        "candidate_reasons",
                        "x",
                        "y",
                        "width",
                        "height",
                        "visible",
                        "client_rect_count",
                        "computed",
                        "box_quads",
                    )
                }
                for candidate in category.get("candidates") or []
            ],
        }
    return {
        "document": {
            key: document.get(key)
            for key in ("ready_state", "element_count")
        },
        "geometry": {
            key: geometry.get(key)
            for key in (
                "inner_width",
                "inner_height",
                "scroll_x",
                "scroll_y",
                "device_pixel_ratio",
                "document_client_width",
                "document_client_height",
                "document_scroll_width",
                "document_scroll_height",
                "body_client_width",
                "body_client_height",
                "body_scroll_width",
                "body_scroll_height",
                "visual_viewport",
            )
        },
        "fonts": {
            key: fonts.get(key)
            for key in ("supported", "status", "face_count", "ready_at_sample")
        },
        "images": {
            key: images.get(key)
            for key in (
                "total",
                "complete",
                "complete_with_pixels",
                "complete_without_pixels",
                "pending",
                "lazy",
            )
        },
        "geometry_probes": probes,
        "feature_probes": {
            "scanned_elements": feature_probes.get("scanned_elements"),
            "comparable_scanned_elements": feature_probes.get(
                "comparable_scanned_elements"
            ),
            "scan_truncated": feature_probes.get("scan_truncated"),
            "total_elements": feature_probes.get("total_elements"),
            "categories": feature_categories,
        },
    }


def capture_chromium_image(
    page, screenshot, geometry_selectors=None, cdp_session=None
):
    """Bracket one Chromium PNG with synchronous capture-critical samples."""
    state = capture_chromium_state(page, geometry_selectors)
    prepare_chromium_feature_probes(state, cdp_session)
    page.screenshot(
        path=str(screenshot),
        full_page=False,
        timeout=50000,
    )
    post_capture_state = capture_chromium_state(page, geometry_selectors)
    prepare_chromium_feature_probes(post_capture_state, cdp_session)
    before_signature = chromium_capture_signature(state)
    after_signature = chromium_capture_signature(post_capture_state)
    post_capture_state.pop("_hash_sources", None)
    boundary = {
        "bracketed": True,
        "stable": before_signature == after_signature,
        "before": before_signature,
        "after": after_signature,
    }
    finalize_chromium_state_hashes(state)
    return state, boundary


def warm_chromium_capture(page):
    """Resolve paint-time resources before capture-boundary state sampling."""
    page.screenshot(full_page=False, timeout=50000)
    # Yield one bounded browser task turn, matching the Obscura CLI warm-up.
    page.wait_for_timeout(1)
    return {
        "performed": True,
        "discardedShots": 1,
        "taskTurnMs": 1,
        "phase": RESOURCE_WARMUP_PHASE,
    }


def freeze_chromium_animations(page, sample_ms):
    """Pause currently exposed Web Animations at one explicit local time.

    This is opt-in: live wall-clock captures remain useful for runtime
    failures, while deterministic CSS-animation comparisons need Chromium
    sampled at the same T=0 used by the static renderer.
    """
    return page.evaluate(
        """sampleMs => {
          if (typeof document.getAnimations !== "function") {
            return {
              supported: false,
              requested_ms: sampleMs,
              discovered: 0,
              frozen: 0,
              failures: []
            };
          }
          const animations = document.getAnimations();
          const failures = [];
          let frozen = 0;
          for (let index = 0; index < animations.length; index++) {
            const animation = animations[index];
            try {
              animation.pause();
              animation.currentTime = sampleMs;
              frozen++;
            } catch (error) {
              failures.push({
                index,
                name: error && error.name ? String(error.name) : "Error",
                message: error && error.message
                  ? String(error.message)
                  : String(error)
              });
            }
          }
          if (document.documentElement) {
            void document.documentElement.getBoundingClientRect().width;
          }
          return {
            supported: true,
            requested_ms: sampleMs,
            discovered: animations.length,
            frozen,
            failures
          };
        }""",
        sample_ms,
    )


def load_rgb(path):
    return np.asarray(Image.open(path).convert("RGB"))


def write_results(path, manifest):
    path.write_text(json.dumps(manifest, indent=2) + "\n")


def compare_page_states(obscura, chromium):
    """Return explicit same-page and geometry deltas; never infer a parity verdict."""
    obscura_document = (obscura or {}).get("document") or {}
    chromium_document = (chromium or {}).get("document") or {}
    obscura_geometry = (obscura or {}).get("geometry") or {}
    chromium_geometry = (chromium or {}).get("geometry") or {}

    def delta(left, right):
        if isinstance(left, (int, float)) and isinstance(right, (int, float)):
            return left - right
        return None

    geometry_fields = (
        "inner_width",
        "inner_height",
        "scroll_x",
        "scroll_y",
        "document_client_width",
        "document_client_height",
        "document_scroll_width",
        "document_scroll_height",
        "body_client_width",
        "body_client_height",
        "body_scroll_width",
        "body_scroll_height",
    )
    return {
        "url_equal": (obscura or {}).get("url") == (chromium or {}).get("url"),
        "ready_state_equal": (
            obscura_document.get("ready_state")
            == chromium_document.get("ready_state")
        ),
        "element_count_delta": delta(
            obscura_document.get("element_count"),
            chromium_document.get("element_count"),
        ),
        "outer_html_utf16_delta": delta(
            obscura_document.get("outer_html_utf16"),
            chromium_document.get("outer_html_utf16"),
        ),
        "body_text_utf16_delta": delta(
            obscura_document.get("body_text_utf16"),
            chromium_document.get("body_text_utf16"),
        ),
        "outer_html_fingerprint_equal": (
            obscura_document.get("outer_html_fnv1a32") is not None
            and obscura_document.get("outer_html_fnv1a32")
            == chromium_document.get("outer_html_fnv1a32")
        ),
        "normalized_outer_html_utf16_delta": delta(
            obscura_document.get("normalized_outer_html_utf16"),
            chromium_document.get("normalized_outer_html_utf16"),
        ),
        "normalized_outer_html_fingerprint_equal": (
            obscura_document.get("normalized_outer_html_fnv1a32") is not None
            and obscura_document.get("normalized_outer_html_fnv1a32")
            == chromium_document.get("normalized_outer_html_fnv1a32")
        ),
        "body_text_fingerprint_equal": (
            obscura_document.get("body_text_fnv1a32") is not None
            and obscura_document.get("body_text_fnv1a32")
            == chromium_document.get("body_text_fnv1a32")
        ),
        "geometry_delta": {
            field: delta(
                obscura_geometry.get(field),
                chromium_geometry.get(field),
            )
            for field in geometry_fields
        },
    }


def classify_state_comparability(
    obscura, chromium, chromium_capture_boundary=None
):
    """Classify whether two screenshots represent comparable live page states.

    Exact DOM/text hashes are intentionally not inputs. Different engines can
    serialize equivalent live DOMs differently. Instead, this uses bounded,
    coarse provenance signals that identify gross incomplete-route/load states
    without turning small DOM differences into rendering exclusions.
    """
    obscura = obscura or {}
    chromium = chromium or {}
    obscura_document = obscura.get("document") or {}
    chromium_document = chromium.get("document") or {}

    def count_signal(left, right, minimum_max, minimum_delta, ratio_limit):
        if not (
            isinstance(left, (int, float))
            and not isinstance(left, bool)
            and isinstance(right, (int, float))
            and not isinstance(right, bool)
        ):
            return {
                "available": False,
                "obscura": left,
                "chromium": right,
                "gross_difference": False,
                "catastrophic_difference": False,
            }
        maximum = max(left, right)
        minimum = min(left, right)
        difference = abs(left - right)
        ratio = minimum / maximum if maximum else 1.0
        return {
            "available": True,
            "obscura": left,
            "chromium": right,
            "absolute_difference": difference,
            "smaller_to_larger_ratio": round(ratio, 6),
            "gross_difference": (
                maximum >= minimum_max
                and difference >= minimum_delta
                and ratio < ratio_limit
            ),
            "catastrophic_difference": (
                maximum >= minimum_max * 2
                and difference >= minimum_delta * 2
                and ratio < 0.2
            ),
        }

    element_signal = count_signal(
        obscura_document.get("element_count"),
        chromium_document.get("element_count"),
        minimum_max=20,
        minimum_delta=15,
        ratio_limit=0.6,
    )
    text_signal = count_signal(
        obscura_document.get("body_text_utf16"),
        chromium_document.get("body_text_utf16"),
        minimum_max=256,
        minimum_delta=256,
        ratio_limit=0.6,
    )

    obscura_probes = obscura.get("geometry_probes") or []
    chromium_probes = chromium.get("geometry_probes") or []
    probe_pairs = []
    for index in range(min(len(obscura_probes), len(chromium_probes))):
        left_probe = obscura_probes[index] or {}
        right_probe = chromium_probes[index] or {}
        left = left_probe.get("count")
        right = right_probe.get("count")
        if (
            left_probe.get("valid") is not False
            and right_probe.get("valid") is not False
            and isinstance(left, int)
            and not isinstance(left, bool)
            and isinstance(right, int)
            and not isinstance(right, bool)
        ):
            maximum = max(left, right)
            minimum = min(left, right)
            ratio = minimum / maximum if maximum else 1.0
            probe_pairs.append(
                {
                    "index": index,
                    "selector": left_probe.get("selector")
                    or right_probe.get("selector"),
                    "obscura": left,
                    "chromium": right,
                    "absolute_difference": abs(left - right),
                    "smaller_to_larger_ratio": round(ratio, 6),
                }
            )
    obscura_probe_total = sum(pair["obscura"] for pair in probe_pairs)
    chromium_probe_total = sum(pair["chromium"] for pair in probe_pairs)
    maximum_probe_total = max(obscura_probe_total, chromium_probe_total)
    probe_absolute_difference = sum(
        pair["absolute_difference"] for pair in probe_pairs
    )
    gross_probe_pairs = sum(
        1
        for pair in probe_pairs
        if max(pair["obscura"], pair["chromium"]) >= 3
        and pair["smaller_to_larger_ratio"] < 0.5
    )
    structural_signal = {
        "available": bool(probe_pairs),
        "pairs_compared": len(probe_pairs),
        "obscura_total": obscura_probe_total,
        "chromium_total": chromium_probe_total,
        "summed_absolute_difference": probe_absolute_difference,
        "gross_pair_count": gross_probe_pairs,
        "gross_difference": bool(probe_pairs)
        and (
            gross_probe_pairs >= 2
            or (
                maximum_probe_total >= 6
                and probe_absolute_difference
                >= max(4, int(maximum_probe_total * 0.35 + 0.999999))
            )
        ),
    }

    obscura_boundary_stable = obscura.get(
        "state_and_screenshot_share_capture_boundary"
    )
    chromium_boundary_stable = (chromium_capture_boundary or {}).get("stable")
    boundary_instability = (
        obscura_boundary_stable is False or chromium_boundary_stable is False
    )
    url_mismatch = (
        bool(obscura.get("url"))
        and bool(chromium.get("url"))
        and obscura.get("url") != chromium.get("url")
    )
    gross_signals = [
        name
        for name, signal in (
            ("element-count", element_signal),
            ("body-text-length", text_signal),
            ("structural-probe-counts", structural_signal),
        )
        if signal["gross_difference"]
    ]
    catastrophic_signals = [
        name
        for name, signal in (
            ("element-count", element_signal),
            ("body-text-length", text_signal),
        )
        if signal.get("catastrophic_difference")
    ]
    available_provenance = sum(
        bool(signal["available"])
        for signal in (element_signal, text_signal, structural_signal)
    )

    reasons = []
    if boundary_instability:
        reasons.append("capture-boundary-instability")
    if url_mismatch:
        reasons.append("final-url-mismatch")
    if catastrophic_signals:
        reasons.append(
            "catastrophic-provenance-difference:"
            + ",".join(catastrophic_signals)
        )
    elif len(gross_signals) >= 2:
        reasons.append(
            "multiple-gross-provenance-differences:"
            + ",".join(gross_signals)
        )
    if available_provenance == 0:
        reasons.append("insufficient-state-provenance")

    comparable = not reasons
    if comparable:
        classification = "comparable"
    elif boundary_instability:
        classification = "capture-boundary-unstable"
    elif available_provenance == 0:
        classification = "insufficient-provenance"
    else:
        classification = "different-live-state"
    return {
        "state_comparable": comparable,
        "classification": classification,
        "reasons": reasons,
        "gross_provenance_signals": gross_signals,
        "evidence": {
            "capture_boundary": {
                "obscura_shared": obscura_boundary_stable,
                "chromium_stable": chromium_boundary_stable,
            },
            "url_mismatch": url_mismatch,
            "element_count": element_signal,
            "body_text_utf16": text_signal,
            "structural_probe_counts": structural_signal,
        },
        "hashes_used_for_classification": False,
    }


def classify_fidelity_metric(
    capture_purpose, state_comparable, metrics_present, metrics=None
):
    """Return whether raw image metrics are valid representative evidence."""
    reasons = []
    if capture_purpose != "representative-fidelity":
        reasons.append("cold-load-latency-mode")
    if state_comparable is not True:
        reasons.append("page-state-not-comparable")
    if not metrics_present:
        reasons.append("image-metrics-unavailable")
    metrics = metrics or {}
    contentless_pair = (
        metrics.get("ours_structural_edge_pixels") == 0
        and metrics.get("chromium_structural_edge_pixels") == 0
        and isinstance(metrics.get("ours_luminance_stddev"), (int, float))
        and isinstance(metrics.get("chromium_luminance_stddev"), (int, float))
        and metrics["ours_luminance_stddev"] <= 0.5
        and metrics["chromium_luminance_stddev"] <= 0.5
    )
    if contentless_pair:
        reasons.append("contentless-image-pair")
    return {
        "fidelity_metric_valid": not reasons,
        "exclusion_reasons": reasons,
    }


def canonical_geometry_dom_structure(dom):
    """Return a stable, style-independent form of a bounded DOM descriptor."""
    if not isinstance(dom, dict):
        return None

    def class_tokens(value):
        if not isinstance(value, str):
            return []
        return sorted(set(value.split()))

    def canonical_node(node):
        if not isinstance(node, dict):
            return None
        return {
            "parent_index": node.get("parent_index"),
            "tag": node.get("tag"),
            "id": node.get("id") or "",
            "class_tokens": class_tokens(node.get("class_name")),
            "child_element_count": node.get("child_element_count"),
        }

    subtree = dom.get("subtree")
    if isinstance(subtree, list):
        return {
            "source": "bounded-subtree",
            "subtree_element_count": dom.get("subtree_element_count"),
            "subtree_truncated": bool(dom.get("subtree_truncated")),
            "nodes": [canonical_node(node) for node in subtree],
        }

    # Pre-gate reports only sampled direct children. They are useful raw
    # provenance but cannot establish that deeper target subtrees are equal.
    return None


GENERATED_ID_NAMESPACE_PATTERN = re.compile(
    r"(?:^|[-_:])(ng|ngb|cdk|mat|ember|react|radix|headlessui|mui)(?:[-_:]|$)",
    re.IGNORECASE,
)


def split_generated_id_variance(obscura_id, chromium_id):
    """Describe one structured ID mismatch without deciding it is volatile."""
    if not obscura_id or not chromium_id or obscura_id == chromium_id:
        return None

    prefix_length = 0
    prefix_limit = min(len(obscura_id), len(chromium_id))
    while (
        prefix_length < prefix_limit
        and obscura_id[prefix_length] == chromium_id[prefix_length]
    ):
        prefix_length += 1

    suffix_length = 0
    suffix_limit = min(
        len(obscura_id) - prefix_length,
        len(chromium_id) - prefix_length,
    )
    while (
        suffix_length < suffix_limit
        and obscura_id[-1 - suffix_length] == chromium_id[-1 - suffix_length]
    ):
        suffix_length += 1

    # Do not let a coincidentally shared salt character become part of the
    # stable suffix (`...-7-panel` versus `...-17-panel`). Stable structured
    # suffixes begin at an ID separator; otherwise the whole tail remains salt.
    if suffix_length:
        raw_suffix = obscura_id[len(obscura_id) - suffix_length :]
        if raw_suffix[0] not in "-_:":
            separator_offsets = [
                raw_suffix.find(separator)
                for separator in ("-", "_", ":")
                if raw_suffix.find(separator) >= 0
            ]
            suffix_length = (
                len(raw_suffix) - min(separator_offsets) if separator_offsets else 0
            )

    suffix_start_obscura = len(obscura_id) - suffix_length
    suffix_start_chromium = len(chromium_id) - suffix_length
    prefix = obscura_id[:prefix_length]
    suffix = obscura_id[suffix_start_obscura:] if suffix_length else ""
    obscura_salt = obscura_id[prefix_length:suffix_start_obscura]
    chromium_salt = chromium_id[prefix_length:suffix_start_chromium]
    if not obscura_salt or not chromium_salt:
        return None

    stable_fingerprint = prefix + "<volatile-id-salt>" + suffix
    generated_namespace = GENERATED_ID_NAMESPACE_PATTERN.search(prefix + suffix)
    salt_is_generated = (
        generated_namespace is not None
        and any(character.isdigit() for character in obscura_salt)
        and any(character.isdigit() for character in chromium_salt)
        and len(obscura_salt) <= 32
        and len(chromium_salt) <= 32
    )
    return {
        "prefix": prefix,
        "suffix": suffix,
        "obscura_salt": obscura_salt,
        "chromium_salt": chromium_salt,
        "normalized_fingerprint": stable_fingerprint,
        "generated_namespace": (
            generated_namespace.group(1).lower() if generated_namespace else None
        ),
        "generated_salt_candidate": salt_is_generated,
    }


def compare_geometry_dom_ids(obscura_nodes, chromium_nodes):
    """Compare IDs, allowing only repeated framework-generated salt changes."""
    if len(obscura_nodes) != len(chromium_nodes):
        return {
            "comparable": False,
            "mismatches": [],
            "normalized_mismatch_count": 0,
            "semantic_mismatch_count": 0,
        }

    mismatches = []
    mapping_fingerprints = {}
    for index, (obscura_node, chromium_node) in enumerate(
        zip(obscura_nodes, chromium_nodes)
    ):
        obscura_id = (obscura_node or {}).get("id") or ""
        chromium_id = (chromium_node or {}).get("id") or ""
        if obscura_id == chromium_id:
            continue
        variance = split_generated_id_variance(obscura_id, chromium_id)
        mismatch = {
            "node_index": index,
            "obscura_id": obscura_id,
            "chromium_id": chromium_id,
            "normalized_as_volatile": False,
            "variance": variance,
        }
        mismatches.append(mismatch)
        if variance and variance["generated_salt_candidate"]:
            mapping = (variance["obscura_salt"], variance["chromium_salt"])
            mapping_fingerprints.setdefault(mapping, set()).add(
                variance["normalized_fingerprint"]
            )

    # A generated-looking numeric difference is not enough by itself. The same
    # salt substitution must recur in at least two distinct structured IDs,
    # such as `ngb-nav-7` and `ngb-nav-7-panel`. This keeps one-off semantic
    # identifiers authoritative while tolerating framework instance counters.
    for mismatch in mismatches:
        variance = mismatch["variance"]
        if not variance or not variance["generated_salt_candidate"]:
            continue
        mapping = (variance["obscura_salt"], variance["chromium_salt"])
        mismatch["normalized_as_volatile"] = (
            len(mapping_fingerprints.get(mapping, ())) >= 2
        )

    normalized_count = sum(
        mismatch["normalized_as_volatile"] for mismatch in mismatches
    )
    semantic_count = len(mismatches) - normalized_count
    return {
        "comparable": semantic_count == 0,
        "mismatches": mismatches,
        "normalized_mismatch_count": normalized_count,
        "semantic_mismatch_count": semantic_count,
    }


def geometry_dom_topology(structure):
    """Return the canonical descriptor with raw diagnostic IDs removed."""
    if not isinstance(structure, dict):
        return None
    return {
        "source": structure.get("source"),
        "subtree_element_count": structure.get("subtree_element_count"),
        "subtree_truncated": structure.get("subtree_truncated"),
        "nodes": [
            {key: value for key, value in (node or {}).items() if key != "id"}
            for node in structure.get("nodes", [])
        ],
    }


def compare_geometry_dom_structures(obscura_dom, chromium_dom):
    """Classify whether two geometry rects address the same DOM structure."""
    obscura_structure = canonical_geometry_dom_structure(obscura_dom)
    chromium_structure = canonical_geometry_dom_structure(chromium_dom)
    available = obscura_structure is not None and chromium_structure is not None
    topology_equal = available and geometry_dom_topology(
        obscura_structure
    ) == geometry_dom_topology(chromium_structure)
    id_comparison = (
        compare_geometry_dom_ids(
            obscura_structure.get("nodes", []),
            chromium_structure.get("nodes", []),
        )
        if available and topology_equal
        else None
    )
    structures_equal = topology_equal and id_comparison["comparable"]
    truncated = available and (
        obscura_structure.get("subtree_truncated") is True
        or chromium_structure.get("subtree_truncated") is True
    )
    comparable = structures_equal and not truncated
    reasons = []
    if not available:
        reasons.append("target-subtree-descriptor-unavailable")
    elif not structures_equal:
        reasons.append("target-subtree-structure-mismatch")
    elif truncated:
        reasons.append("target-subtree-descriptor-truncated")
    return {
        "available": available,
        "comparable": comparable,
        "classification": (
            "comparable"
            if comparable
            else "insufficient-target-structure"
            if not available or truncated
            else "different-target-structure"
        ),
        "reasons": reasons,
        "topology_equal": topology_equal,
        "id_comparison": id_comparison,
        "obscura": obscura_structure,
        "chromium": chromium_structure,
    }


def geometry_verdict_exclusions(comparisons):
    """Return selector-scoped exclusions without changing full-page fidelity."""
    return [
        {
            "index": comparison.get("index"),
            "selector": comparison.get("selector"),
            "reasons": comparison.get("geometry_verdict_exclusion_reasons") or [],
        }
        for comparison in comparisons or []
        if comparison.get("geometry_verdict_valid") is not True
    ]


def summarize_geometry_verdicts(comparisons):
    """Summarize selector-level eligibility without folding it into pixels."""
    comparisons = comparisons or []
    valid = [
        comparison
        for comparison in comparisons
        if comparison.get("geometry_verdict_valid") is True
    ]
    exclusions = geometry_verdict_exclusions(comparisons)
    return {
        "valid_selectors": len(valid),
        "excluded_selectors": len(exclusions),
        "all_selectors_valid": bool(comparisons) and not exclusions,
        "excluded": exclusions,
    }


def compare_geometry_probes(obscura, chromium):
    """Report raw deltas and gate geometry verdicts on target structure."""
    obscura_probes = (obscura or {}).get("geometry_probes") or []
    chromium_probes = (chromium or {}).get("geometry_probes") or []
    comparisons = []
    for index in range(max(len(obscura_probes), len(chromium_probes))):
        obscura_probe = (
            obscura_probes[index] if index < len(obscura_probes) else None
        )
        chromium_probe = (
            chromium_probes[index] if index < len(chromium_probes) else None
        )
        obscura_rects = (obscura_probe or {}).get("rects") or []
        chromium_rects = (chromium_probe or {}).get("rects") or []
        rect_deltas = []
        subtree_comparisons = []
        for rect_index in range(min(len(obscura_rects), len(chromium_rects))):
            obscura_rect = obscura_rects[rect_index]
            chromium_rect = chromium_rects[rect_index]
            subtree_comparison = compare_geometry_dom_structures(
                obscura_rect.get("dom"), chromium_rect.get("dom")
            )
            subtree_comparisons.append(subtree_comparison)
            deltas = {}
            for field in ("x", "y", "width", "height"):
                left = obscura_rect.get(field)
                right = chromium_rect.get(field)
                deltas[field] = (
                    left - right
                    if isinstance(left, (int, float))
                    and isinstance(right, (int, float))
                    else None
                )
            obscura_computed = obscura_rect.get("computed") or {}
            chromium_computed = chromium_rect.get("computed") or {}
            computed_differences = {
                field: {
                    "obscura": obscura_computed.get(field),
                    "chromium": chromium_computed.get(field),
                }
                for field in sorted(
                    set(obscura_computed) | set(chromium_computed)
                )
                if obscura_computed.get(field) != chromium_computed.get(field)
            }
            rect_deltas.append(
                {
                    "index": rect_index,
                    "delta": deltas,
                    "visibility": {
                        "obscura": obscura_rect.get("visible"),
                        "chromium": chromium_rect.get("visible"),
                    },
                    "target_subtree_comparability": subtree_comparison,
                    "geometry_delta_valid": subtree_comparison["comparable"],
                    "computed_difference_count": len(computed_differences),
                    "computed_differences": computed_differences,
                }
            )
        obscura_count = (obscura_probe or {}).get("count")
        chromium_count = (chromium_probe or {}).get("count")
        selector_left = (obscura_probe or {}).get("selector")
        selector_right = (chromium_probe or {}).get("selector")
        query_valid = (
            (obscura_probe or {}).get("valid") is True
            and (chromium_probe or {}).get("valid") is True
        )
        counts_equal = (
            isinstance(obscura_count, int)
            and not isinstance(obscura_count, bool)
            and isinstance(chromium_count, int)
            and not isinstance(chromium_count, bool)
            and obscura_count == chromium_count
        )
        verdict_exclusions = []
        if not query_valid:
            verdict_exclusions.append("selector-query-invalid")
        if selector_left != selector_right:
            verdict_exclusions.append("selector-mismatch")
        if not counts_equal:
            verdict_exclusions.append("target-count-mismatch")
        elif obscura_count == 0:
            verdict_exclusions.append("no-matched-targets")
        if counts_equal and isinstance(obscura_count, int) and obscura_count > 0:
            expected_rects = min(
                obscura_count,
                (obscura_probe or {}).get("rect_limit")
                or GEOMETRY_PROBE_RECT_LIMIT,
            )
            if len(subtree_comparisons) != expected_rects:
                verdict_exclusions.append("paired-target-descriptors-incomplete")
        subtree_reasons = {
            reason
            for comparison in subtree_comparisons
            for reason in comparison["reasons"]
        }
        verdict_exclusions.extend(sorted(subtree_reasons))
        # Preserve reason order while avoiding duplicates.
        verdict_exclusions = list(dict.fromkeys(verdict_exclusions))
        geometry_verdict_valid = not verdict_exclusions
        comparisons.append(
            {
                "index": index,
                "selector": (
                    (obscura_probe or {}).get("selector")
                    if obscura_probe is not None
                    else (chromium_probe or {}).get("selector")
                ),
                "valid": {
                    "obscura": (obscura_probe or {}).get("valid"),
                    "chromium": (chromium_probe or {}).get("valid"),
                },
                "geometry_verdict_valid": geometry_verdict_valid,
                "geometry_verdict_exclusion_reasons": verdict_exclusions,
                "target_subtree_comparability": {
                    "pairs_compared": len(subtree_comparisons),
                    "all_comparable": bool(subtree_comparisons)
                    and all(
                        comparison["comparable"]
                        for comparison in subtree_comparisons
                    ),
                    "bounded_descriptor": (
                        "tag/id/class tokens, child counts, and parent-indexed "
                        "descendant structure"
                    ),
                },
                "errors": {
                    "obscura": (obscura_probe or {}).get("error"),
                    "chromium": (chromium_probe or {}).get("error"),
                },
                "counts": {
                    "obscura": obscura_count,
                    "chromium": chromium_count,
                    "delta": (
                        obscura_count - chromium_count
                        if isinstance(obscura_count, int)
                        and isinstance(chromium_count, int)
                        else None
                    ),
                },
                "rects_compared": len(rect_deltas),
                "rect_deltas": rect_deltas,
            }
        )
    return comparisons


def compare_feature_probes(obscura, chromium):
    """Compare selector-free candidates by category and document element index."""
    obscura_features = (obscura or {}).get("feature_probes") or {}
    chromium_features = (chromium or {}).get("feature_probes") or {}
    obscura_categories = obscura_features.get("categories") or {}
    chromium_categories = chromium_features.get("categories") or {}

    def numeric_delta(left, right):
        if isinstance(left, (int, float)) and isinstance(right, (int, float)):
            return left - right
        return None

    def quad_delta(left, right):
        if not (
            isinstance(left, list)
            and isinstance(right, list)
            and len(left) == len(right) == 8
            and all(isinstance(value, (int, float)) for value in left + right)
        ):
            return None
        return [left[index] - right[index] for index in range(8)]

    category_comparisons = []
    for kind in sorted(set(obscura_categories) | set(chromium_categories)):
        obscura_category = obscura_categories.get(kind) or {}
        chromium_category = chromium_categories.get(kind) or {}
        obscura_candidates = {
            candidate.get("comparison_index", candidate.get("dom_index")): candidate
            for candidate in obscura_category.get("candidates") or []
            if isinstance(
                candidate.get("comparison_index", candidate.get("dom_index")), int
            )
        }
        chromium_candidates = {
            candidate.get("comparison_index", candidate.get("dom_index")): candidate
            for candidate in chromium_category.get("candidates") or []
            if isinstance(
                candidate.get("comparison_index", candidate.get("dom_index")), int
            )
        }
        candidate_comparisons = []
        for comparison_index in sorted(
            set(obscura_candidates) | set(chromium_candidates)
        ):
            obscura_candidate = obscura_candidates.get(comparison_index)
            chromium_candidate = chromium_candidates.get(comparison_index)
            obscura_computed = (obscura_candidate or {}).get("computed") or {}
            chromium_computed = (chromium_candidate or {}).get("computed") or {}
            computed_differences = {
                field: {
                    "obscura": obscura_computed.get(field),
                    "chromium": chromium_computed.get(field),
                }
                for field in sorted(set(obscura_computed) | set(chromium_computed))
                if obscura_computed.get(field) != chromium_computed.get(field)
            }
            obscura_quads = (obscura_candidate or {}).get("box_quads") or {}
            chromium_quads = (chromium_candidate or {}).get("box_quads") or {}
            quad_spaces_equal = (
                obscura_quads.get("coordinate_space") is not None
                and obscura_quads.get("coordinate_space")
                == chromium_quads.get("coordinate_space")
            )
            candidate_comparisons.append(
                {
                    "comparison_index": comparison_index,
                    "dom_index": {
                        "obscura": (obscura_candidate or {}).get("dom_index"),
                        "chromium": (chromium_candidate or {}).get("dom_index"),
                    },
                    "present": {
                        "obscura": obscura_candidate is not None,
                        "chromium": chromium_candidate is not None,
                    },
                    "candidate_reasons": {
                        "obscura": (obscura_candidate or {}).get(
                            "candidate_reasons"
                        ),
                        "chromium": (chromium_candidate or {}).get(
                            "candidate_reasons"
                        ),
                    },
                    "geometry_delta": {
                        field: numeric_delta(
                            (obscura_candidate or {}).get(field),
                            (chromium_candidate or {}).get(field),
                        )
                        for field in ("x", "y", "width", "height")
                    },
                    "visibility": {
                        "obscura": (obscura_candidate or {}).get("visible"),
                        "chromium": (chromium_candidate or {}).get("visible"),
                    },
                    "computed_difference_count": len(computed_differences),
                    "computed_differences": computed_differences,
                    "box_quads": {
                        "obscura_available": bool(obscura_quads),
                        "chromium_available": bool(chromium_quads),
                        "coordinate_spaces_equal": quad_spaces_equal,
                        "content_delta": (
                            quad_delta(
                                obscura_quads.get("content"),
                                chromium_quads.get("content"),
                            )
                            if quad_spaces_equal
                            else None
                        ),
                        "border_delta": (
                            quad_delta(
                                obscura_quads.get("border"),
                                chromium_quads.get("border"),
                            )
                            if quad_spaces_equal
                            else None
                        ),
                    },
                }
            )
        obscura_seen = obscura_category.get("matches_seen")
        chromium_seen = chromium_category.get("matches_seen")
        category_comparisons.append(
            {
                "kind": kind,
                "matches_seen": {
                    "obscura": obscura_seen,
                    "chromium": chromium_seen,
                    "delta": numeric_delta(obscura_seen, chromium_seen),
                },
                "candidates_truncated": {
                    "obscura": obscura_category.get("candidates_truncated"),
                    "chromium": chromium_category.get("candidates_truncated"),
                },
                "candidates": candidate_comparisons,
            }
        )
    return {
        "bounds": {
            "obscura": obscura_features.get("bounds"),
            "chromium": chromium_features.get("bounds"),
        },
        "scan": {
            "obscura_scanned_elements": obscura_features.get("scanned_elements"),
            "chromium_scanned_elements": chromium_features.get("scanned_elements"),
            "obscura_comparable_scanned_elements": obscura_features.get(
                "comparable_scanned_elements"
            ),
            "chromium_comparable_scanned_elements": chromium_features.get(
                "comparable_scanned_elements"
            ),
            "obscura_truncated": obscura_features.get("scan_truncated"),
            "chromium_truncated": chromium_features.get("scan_truncated"),
        },
        "box_quad_capabilities": {
            "obscura": obscura_features.get("box_quads"),
            "chromium": chromium_features.get("box_quads"),
        },
        "categories": category_comparisons,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("urls", help="one URL per line; # comments allowed")
    parser.add_argument("--obscura-bin", required=True)
    parser.add_argument("--baseline-bin")
    parser.add_argument(
        "--chromium-bin",
        help="Chromium executable (default: Playwright's pinned Chromium)",
    )
    parser.add_argument("--out", required=True, help="must not already exist")
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--height", type=int, default=1400)
    parser.add_argument("--settle-ms", type=int, default=3000)
    parser.add_argument(
        "--capture-purpose",
        choices=["representative-fidelity", "cold-load-latency"],
        default="representative-fidelity",
        help=(
            "representative-fidelity requires a non-zero settle; "
            "cold-load-latency requires --settle-ms=0 and excludes pixel "
            "metrics from fidelity interpretation"
        ),
    )
    parser.add_argument(
        "--animation-time-ms",
        type=int,
        choices=[0],
        help=(
            "pause Chromium animations currently exposed through Web Animations "
            "at T=0 immediately before state sampling and screenshot paint; "
            "matches Obscura's deterministic static animation sample"
        ),
    )
    parser.add_argument(
        "--geometry-selector",
        action="append",
        default=[],
        metavar="CSS_SELECTOR",
        help=(
            "repeatable selector whose match count and viewport-relative "
            "bounding rects are sampled in both engines immediately before capture"
        ),
    )
    parser.add_argument(
        "--scroll-x",
        type=int,
        help="scroll both engines to this CSS-pixel x offset before capture",
    )
    parser.add_argument(
        "--scroll-y",
        type=parse_scroll_y,
        help="scroll both engines to this CSS-pixel y offset or 'bottom' before capture",
    )
    args = parser.parse_args()
    if args.settle_ms % 1000:
        parser.error(
            "--settle-ms must be a whole number of seconds because Obscura's "
            "fetch --wait interface accepts integer seconds"
        )
    if args.settle_ms < 0:
        parser.error("--settle-ms must be non-negative")
    if args.settle_ms == 0 and args.capture_purpose != "cold-load-latency":
        parser.error(
            "--settle-ms=0 is cold-load latency mode; pass "
            "--capture-purpose=cold-load-latency explicitly"
        )
    if args.capture_purpose == "cold-load-latency" and args.settle_ms != 0:
        parser.error("cold-load-latency requires --settle-ms=0")

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=False)
    controlled_scroll = None
    if args.scroll_x is not None or args.scroll_y is not None:
        controlled_scroll = (
            args.scroll_x if args.scroll_x is not None else 0,
            args.scroll_y if args.scroll_y is not None else 0,
        )
    urls = [
        line.strip()
        for line in Path(args.urls).read_text().splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    manifest = {
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "viewport": {"width": args.width, "height": args.height, "dpr": 1},
        "capture_purpose": args.capture_purpose,
        "fidelity_metrics_enabled": (
            args.capture_purpose == "representative-fidelity"
        ),
        "capture_purpose_semantics": (
            "settled representative capture; raw image metrics are eligible "
            "for fidelity interpretation only when page-state provenance is "
            "also comparable"
            if args.capture_purpose == "representative-fidelity"
            else "zero-settle cold-load latency diagnostic; raw screenshots "
            "and metrics are retained, but are never fidelity evidence"
        ),
        "settle_ms_after_load": args.settle_ms,
        "settle_ms_after_controlled_scroll": (
            args.settle_ms if controlled_scroll is not None else 0
        ),
        "settle_semantics": (
            "full wall-clock interval while pumping each engine; when a "
            "controlled scroll is requested, the same interval runs once "
            "after load and once after the initial scroll. Both engines record "
            "the resulting anchored offset and then reassert the requested "
            "offset with instant behavior immediately before capture state "
            "and paint. Neither engine performs another settle between state/"
            "selector sampling and screenshot paint"
        ),
        "animation_sampling": (
            {
                "mode": "deterministic-active-web-animations",
                "sample_ms": args.animation_time_ms,
                "chromium": (
                    "document.getAnimations() results are paused and assigned "
                    "currentTime immediately before state and screenshot capture"
                ),
                "obscura": "static renderer animation sample time T=0",
            }
            if args.animation_time_ms is not None
            else {"mode": "live-wall-clock", "sample_ms": None}
        ),
        "controlled_scroll": (
            {"x": controlled_scroll[0], "y": controlled_scroll[1]}
            if controlled_scroll is not None
            else None
        ),
        "navigation_timeout_ms": 50000,
        "obscura": binary_version(args.obscura_bin),
        "baseline": binary_version(args.baseline_bin) if args.baseline_bin else None,
        "capture_identity": {
            "normalized": True,
            "configured_user_agent": CANONICAL_USER_AGENT,
            "configured_platform": CANONICAL_PLATFORM,
            "configured_ua_platform": CANONICAL_UA_PLATFORM,
            "configured_ua_platform_version": CANONICAL_UA_PLATFORM_VERSION,
            "obscura_profile": CANONICAL_OBSCURA_PROFILE,
        },
        "capture_media": {
            "normalized": True,
            "color_scheme": CANONICAL_COLOR_SCHEME,
            "reduced_motion": CANONICAL_REDUCED_MOTION,
            "expected_match_media": EXPECTED_MEDIA_MATCHES,
            "reduced_motion_reason": (
                "Obscura currently models the browser default "
                "(no-preference), so Chromium is pinned to the same state"
            ),
        },
        "state_observability": {
            "capture_boundary_phase": CAPTURE_BOUNDARY_PHASE,
            "resource_warmup_phase": RESOURCE_WARMUP_PHASE,
            "chromium": (
                "same page, one discard paint resolves the retained resource "
                "graph, then one bounded task turn and the final scroll "
                "reassert run before synchronous state sampling. State is "
                "immediately before screenshot. A second synchronous sample "
                "after paint brackets the PNG and reports whether capture-"
                "critical geometry/readiness remained stable. DOM SHA-256 "
                "finalization runs on the host only after paint"
            ),
            "obscura": (
                "the paired-capture CLI takes one discard paint over the same "
                "live page, pumps one bounded task turn, then performs the "
                "final instant scroll reassert before deferred DOM/resource/"
                "selector evaluation. Evaluation, captureState, and screenshot run "
                "consecutively without another settle; captureState records the "
                "exact shared PreparedRender viewport, scroll offset, and "
                "content size used by paint"
            ),
        },
        "methodology_limits": {
            "pixel_metrics": (
                "raw full-canvas diagnostics only; they are a tripwire, not a "
                "fidelity verdict. They remain recorded for every successful "
                "pair, but fidelity_metric_valid excludes cold-load mode and "
                "different or unstable live-page states from interpretation"
            ),
            "controlled_scroll": (
                "The settled pre-reassert offset is diagnostic evidence, not "
                "the comparison coordinate: authored smooth scrolling and "
                "scroll anchoring can legitimately move it while layout above "
                "the viewport changes. The requested offset is reasserted "
                "instantly for final state and paint. CSSOM and screenshot "
                "paint share one resource-aware PreparedRender, but different "
                "content sizes can still clamp the same request differently."
            ),
            "page_state": (
                "DOM/body-text fingerprints and length deltas expose different live "
                "page states. They are provenance tripwires, not proof that "
                "equal states contain equal layout or that unequal serialized "
                "DOM necessarily represents a rendering failure. Normalized DOM "
                "fingerprints exclude only Obscura's explicitly marked external-"
                "stylesheet mirror nodes, because Chromium's CSSOM does not "
                "serialize fetched stylesheet text into outerHTML."
            ),
            "selector_target_state": (
                "Each geometry target carries a bounded, parent-indexed DOM "
                "subtree descriptor. Selector geometry remains raw diagnostic "
                "data when match counts or target structures differ, but it is "
                "explicitly excluded from a geometry verdict. A selector-scoped "
                "exclusion does not by itself invalidate unrelated full-page "
                "fidelity metrics."
            ),
            "resource_readiness": (
                "Both engines take and discard one screenshot before sampling "
                "resource readiness, then yield one bounded task turn. This "
                "warms the exact retained render graph instead of comparing "
                "Obscura's pre-paint state with Chromium's post-load state. "
                "A successful fetch is still not proof of equal decode, selected "
                "source, or paint output."
            ),
            "animation_sampling": (
                "The deterministic option controls only animations still "
                "exposed by document.getAnimations() at capture time. Finished "
                "fill-none animations and script-driven visual state that has "
                "already been discarded cannot be rewound by this harness."
            ),
            "automatic_feature_probes": (
                "Transform and text-truncation candidates are discovered by "
                "bounded document-order scans, not site-specific selectors. "
                "A candidate proves that a relevant computed value won the "
                "cascade, not that its visual effect is correct. Candidate "
                "matching by document element index is meaningful only with "
                "the adjacent DOM/body-text provenance evidence. Obscura's marked "
                "external-stylesheet mirror nodes are excluded from that "
                "comparison index while each engine retains its raw DOM index "
                "for CDP lookup. Nested element "
                "scroll metrics are intentionally excluded because Obscura's "
                "current CLI surface cannot yet provide trustworthy values."
            ),
            "box_quads": (
                "Chromium content and border quads come from CDP "
                "DOM.getBoxModel in viewport CSS pixels. Obscura CLI capture "
                "does not expose an equivalent node-scoped CDP call at the "
                "shared screenshot boundary, so its capability is recorded "
                "as unavailable and candidate quads remain null; bounding "
                "rect corners are never relabeled as quads."
            ),
        },
        "pages": [],
    }
    if args.geometry_selector:
        manifest["geometry_probes"] = {
            "selectors": args.geometry_selector,
            "coordinate_space": "viewport-css-px",
            "rect_limit_per_selector": GEOMETRY_PROBE_RECT_LIMIT,
            "subtree_element_limit_per_rect": GEOMETRY_PROBE_SUBTREE_LIMIT,
            "visibility": (
                "practical rendered-box heuristic: client rect exists, positive "
                "bounding size, display/visibility permit paint, and opacity > 0"
            ),
            "comparison_semantics": (
                "raw per-selector counts, rect deltas in document order, "
                "visibility observations, and exact differences between the "
                "computed values that won each engine's cascade. Geometry "
                "verdict validity additionally requires equal selector counts "
                "and matching bounded tag/id/class/child structure for every "
                "paired target; selector exclusions do not invalidate the "
                "full-page fidelity metric"
            ),
        }
    manifest["automatic_feature_probes"] = {
        "scan_limit": FEATURE_PROBE_SCAN_LIMIT,
        "candidate_limit_per_category": FEATURE_PROBE_CANDIDATE_LIMIT,
        "categories": {
            "transform": (
                "computed non-none transform, translate, rotate, scale, or "
                "perspective"
            ),
            "text_truncation": (
                "computed non-clip text-overflow or non-none "
                "-webkit-line-clamp"
            ),
        },
        "comparison_semantics": (
            "normalized document-index candidate presence, raw viewport AABB deltas, "
            "exact computed-value differences, and box-quad capability/data; "
            "no aggregate parity verdict"
        ),
    }
    manifest["obscura_identity_probe"] = probe_obscura_identity(args.obscura_bin)
    manifest["obscura_css_media_probe"] = probe_obscura_css_media(args.obscura_bin)
    if args.baseline_bin:
        manifest["baseline_identity_probe"] = probe_obscura_identity(args.baseline_bin)
        manifest["baseline_css_media_probe"] = probe_obscura_css_media(
            args.baseline_bin
        )
    results_path = out / "results.json"
    write_results(results_path, manifest)
    probes = [
        ("obscura JS media", manifest["obscura_identity_probe"].get("media_matches_configured")),
        ("obscura CSS media", manifest["obscura_css_media_probe"].get("ok")),
    ]
    if args.baseline_bin:
        probes.extend(
            [
                (
                    "baseline JS media",
                    manifest["baseline_identity_probe"].get("media_matches_configured"),
                ),
                (
                    "baseline CSS media",
                    manifest["baseline_css_media_probe"].get("ok"),
                ),
            ]
        )
    failed_probes = [name for name, passed in probes if not passed]
    if failed_probes:
        print(
            "capture environment mismatch: " + ", ".join(failed_probes),
            file=sys.stderr,
        )
        print(f"probe evidence: {results_path}", file=sys.stderr)
        raise SystemExit(2)

    with sync_playwright() as playwright:
        chromium_executable = args.chromium_bin or playwright.chromium.executable_path
        manifest["chromium_executable"] = chromium_executable
        browser = playwright.chromium.launch(
            executable_path=chromium_executable,
            headless=True,
            args=[
                "--no-sandbox",
                "--hide-scrollbars",
                "--disable-background-networking",
                "--force-device-scale-factor=1",
            ],
        )
        manifest["chromium_version"] = browser.version
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            for index, url in enumerate(urls):
                name = f"{index:03d}-{slug(url)}"
                ours_path = out / f"{name}.obscura.png"
                chrome_path = out / f"{name}.chrome.png"
                baseline_path = out / f"{name}.baseline.png"
                page_result = {
                    "url": url,
                    "name": name,
                    "state_comparable": None,
                    "fidelity_metric_valid": False,
                }
                context = browser.new_context(
                    viewport={"width": args.width, "height": args.height},
                    device_scale_factor=1,
                    user_agent=CANONICAL_USER_AGENT,
                    color_scheme=CANONICAL_COLOR_SCHEME,
                    reduced_motion=CANONICAL_REDUCED_MOTION,
                    locale="en-US",
                    timezone_id="UTC",
                )
                page = context.new_page()
                # Repeat the context-level settings at page scope so a future
                # context refactor cannot silently fall back to host media.
                page.emulate_media(
                    color_scheme=CANONICAL_COLOR_SCHEME,
                    reduced_motion=CANONICAL_REDUCED_MOTION,
                )
                chromium_session = context.new_cdp_session(page)
                chromium_identity_override(chromium_session)
                chrome_messages = []
                page.on("console", lambda message: chrome_messages.append(f"console {message.type}: {message.text}"))
                page.on("pageerror", lambda error: chrome_messages.append(f"pageerror: {error}"))

                ours_future = executor.submit(
                    capture_obscura,
                    args.obscura_bin,
                    url,
                    ours_path,
                    out / f"{name}.obscura.log",
                    args.width,
                    args.height,
                    args.settle_ms,
                    controlled_scroll,
                    args.geometry_selector,
                    args.animation_time_ms,
                )
                baseline_future = None
                if args.baseline_bin:
                    baseline_future = executor.submit(
                        capture_obscura,
                        args.baseline_bin,
                        url,
                        baseline_path,
                        out / f"{name}.baseline.log",
                        args.width,
                        args.height,
                        args.settle_ms,
                        controlled_scroll,
                        args.geometry_selector,
                        args.animation_time_ms,
                    )

                chrome_started = time.time()
                chromium_scroll_state = None
                chromium_capture_boundary = None
                chromium_resource_warmup = None
                try:
                    page.goto(url, wait_until="load", timeout=50000)
                    page.wait_for_timeout(args.settle_ms)
                    if controlled_scroll is not None:
                        scroll_x, scroll_y = controlled_scroll
                        page.evaluate(
                            """([x, y]) => {
                              const requestedY = y === "bottom"
                                ? document.documentElement.scrollHeight
                                : y;
                              window.scrollTo(x, requestedY);
                            }""",
                            [scroll_x, scroll_y],
                        )
                        page.wait_for_timeout(args.settle_ms)
                    animation_sampling = None
                    if args.animation_time_ms is not None:
                        animation_sampling = freeze_chromium_animations(
                            page, args.animation_time_ms
                        )
                    chromium_resource_warmup = warm_chromium_capture(page)
                    if controlled_scroll is not None:
                        chromium_scroll_state = (
                            reassert_chromium_controlled_scroll(
                                page, controlled_scroll
                            )
                        )
                    chromium_state_ok = False
                    try:
                        (
                            chromium_state,
                            chromium_capture_boundary,
                        ) = capture_chromium_image(
                            page,
                            chrome_path,
                            args.geometry_selector,
                            chromium_session,
                        )
                        if not chromium_capture_boundary["stable"]:
                            chrome_messages.append(
                                "capture boundary warning: capture-critical "
                                "state changed while taking the Chromium PNG"
                            )
                        if chromium_scroll_state is not None:
                            chromium_state["controlled_scroll"] = (
                                chromium_scroll_state
                            )
                        if animation_sampling is not None:
                            chromium_state["animation_sampling"] = (
                                animation_sampling
                            )
                        chromium_state["media"]["matches_configured"] = (
                            media_matches_configured(chromium_state["media"])
                        )
                        if not chromium_state["media"]["matches_configured"]:
                            raise RuntimeError(
                                "Chromium matchMedia differs from configured capture media"
                            )
                        chromium_state_ok = True
                    except Exception as error:
                        chromium_state = None
                        chrome_messages.append(f"state capture error: {error}")
                    if not chrome_path.is_file():
                        page.screenshot(
                            path=str(chrome_path),
                            full_page=False,
                            timeout=50000,
                        )
                    chrome_ok = (
                        chromium_state_ok
                        and chrome_path.is_file()
                        and chrome_path.stat().st_size > 0
                    )
                    chrome_status = 0 if chrome_ok else "state-error"
                except Exception as error:
                    chrome_messages.append(f"capture error: {error}")
                    chrome_ok = False
                    chrome_status = "error"
                (out / f"{name}.chrome.log").write_text("\n".join(chrome_messages) + "\n")
                page_result["chromium"] = {
                    "ok": chrome_ok,
                    "status": chrome_status,
                    "elapsed_s": round(time.time() - chrome_started, 3),
                    "title": page.title() if chrome_ok else None,
                    "state": chromium_state if chrome_ok else None,
                    "capture_boundary_validation": (
                        chromium_capture_boundary if chrome_ok else None
                    ),
                    "resource_warmup": (
                        chromium_resource_warmup if chrome_ok else None
                    ),
                    "scroll_state": (
                        chromium_scroll_state if chrome_ok else None
                    ),
                }
                context.close()
                page_result["obscura"] = ours_future.result()
                if baseline_future:
                    page_result["baseline"] = baseline_future.result()

                if chrome_ok and page_result["obscura"]["ok"]:
                    page_result["page_state_comparison"] = compare_page_states(
                        page_result["obscura"].get("state"),
                        chromium_state,
                    )
                    state_comparability = classify_state_comparability(
                        page_result["obscura"].get("state"),
                        chromium_state,
                        chromium_capture_boundary,
                    )
                    page_result["state_comparability"] = state_comparability
                    page_result["state_comparable"] = state_comparability[
                        "state_comparable"
                    ]
                    page_result["feature_probe_comparison"] = (
                        compare_feature_probes(
                            page_result["obscura"].get("state"),
                            chromium_state,
                        )
                    )
                    if args.geometry_selector:
                        geometry_comparison = compare_geometry_probes(
                            page_result["obscura"].get("state"),
                            chromium_state,
                        )
                        page_result["geometry_probe_comparison"] = (
                            geometry_comparison
                        )
                        page_result["geometry_verdict_exclusions"] = (
                            geometry_verdict_exclusions(geometry_comparison)
                        )
                        page_result["geometry_verdict_eligibility"] = (
                            summarize_geometry_verdicts(geometry_comparison)
                        )
                if (
                    chrome_ok
                    and baseline_future
                    and page_result["baseline"]["ok"]
                ):
                    page_result["baseline_page_state_comparison"] = (
                        compare_page_states(
                            page_result["baseline"].get("state"),
                            chromium_state,
                        )
                    )
                    baseline_state_comparability = classify_state_comparability(
                        page_result["baseline"].get("state"),
                        chromium_state,
                        chromium_capture_boundary,
                    )
                    page_result["baseline_state_comparability"] = (
                        baseline_state_comparability
                    )
                    page_result["baseline_state_comparable"] = (
                        baseline_state_comparability["state_comparable"]
                    )
                    page_result["baseline_feature_probe_comparison"] = (
                        compare_feature_probes(
                            page_result["baseline"].get("state"),
                            chromium_state,
                        )
                    )
                    if args.geometry_selector:
                        baseline_geometry_comparison = compare_geometry_probes(
                            page_result["baseline"].get("state"),
                            chromium_state,
                        )
                        page_result["baseline_geometry_probe_comparison"] = (
                            baseline_geometry_comparison
                        )
                        page_result["baseline_geometry_verdict_exclusions"] = (
                            geometry_verdict_exclusions(
                                baseline_geometry_comparison
                            )
                        )
                        page_result["baseline_geometry_verdict_eligibility"] = (
                            summarize_geometry_verdicts(
                                baseline_geometry_comparison
                            )
                        )

                if (
                    controlled_scroll is not None
                    and chrome_ok
                    and page_result["obscura"]["ok"]
                ):
                    ours_scroll = page_result["obscura"].get("scroll_state") or {}
                    ours_actual = ours_scroll.get("actual") or {}
                    ours_content = ours_scroll.get("content") or {}
                    chrome_geometry = chromium_state.get("geometry") or {}
                    comparable = all(
                        isinstance(value, (int, float))
                        for value in (
                            ours_actual.get("x"),
                            ours_actual.get("y"),
                            chrome_geometry.get("scroll_x"),
                            chrome_geometry.get("scroll_y"),
                        )
                    )
                    page_result["controlled_scroll_comparison"] = {
                        "comparable": comparable,
                        "obscura_pre_reassert_actual": ours_scroll.get(
                            "pre_reassert_actual"
                        ),
                        "chromium_pre_reassert_actual": (
                            chromium_scroll_state or {}
                        ).get("pre_reassert_actual"),
                        "obscura_actual": ours_actual,
                        "chromium_actual": {
                            "x": chrome_geometry.get("scroll_x"),
                            "y": chrome_geometry.get("scroll_y"),
                        },
                        "actual_delta": (
                            {
                                "x": ours_actual["x"] - chrome_geometry["scroll_x"],
                                "y": ours_actual["y"] - chrome_geometry["scroll_y"],
                            }
                            if comparable
                            else None
                        ),
                        "content_size_delta": {
                            "width": (
                                ours_content.get("width")
                                - chrome_geometry.get("document_scroll_width")
                                if isinstance(ours_content.get("width"), (int, float))
                                and isinstance(
                                    chrome_geometry.get("document_scroll_width"),
                                    (int, float),
                                )
                                else None
                            ),
                            "height": (
                                ours_content.get("height")
                                - chrome_geometry.get("document_scroll_height")
                                if isinstance(ours_content.get("height"), (int, float))
                                and isinstance(
                                    chrome_geometry.get("document_scroll_height"),
                                    (int, float),
                                )
                                else None
                            ),
                        },
                    }

                if chrome_ok and page_result["obscura"]["ok"]:
                    chrome_rgb = load_rgb(chrome_path)
                    current_metrics = pair_metrics(load_rgb(ours_path), chrome_rgb)
                    page_result["metrics"] = current_metrics
                    if baseline_future and page_result["baseline"]["ok"]:
                        baseline_metrics = pair_metrics(load_rgb(baseline_path), chrome_rgb)
                        page_result["baseline_metrics"] = baseline_metrics
                        for key in (
                            "rgb_mae",
                            "pixels_gt_10",
                            "pixels_gt_50",
                            "edge_bbox_max_delta",
                            "edge_row_projection_delta",
                            "edge_column_projection_delta",
                            "edge_bidirectional_mean_distance_px",
                            "edge_bidirectional_p95_distance_px",
                        ):
                            if key in current_metrics and key in baseline_metrics:
                                if current_metrics[key] is None or baseline_metrics[key] is None:
                                    continue
                                page_result.setdefault("raw_delta_vs_baseline", {})[key] = round(
                                    current_metrics[key] - baseline_metrics[key], 6
                                )
                fidelity_classification = classify_fidelity_metric(
                    args.capture_purpose,
                    page_result.get("state_comparable"),
                    bool(page_result.get("metrics")),
                    page_result.get("metrics"),
                )
                page_result["fidelity_metric_valid"] = fidelity_classification[
                    "fidelity_metric_valid"
                ]
                page_result["fidelity_metric_exclusion_reasons"] = (
                    fidelity_classification["exclusion_reasons"]
                )
                if baseline_future:
                    baseline_fidelity_classification = classify_fidelity_metric(
                        args.capture_purpose,
                        page_result.get("baseline_state_comparable"),
                        bool(page_result.get("baseline_metrics")),
                        page_result.get("baseline_metrics"),
                    )
                    page_result["baseline_fidelity_metric_valid"] = (
                        baseline_fidelity_classification["fidelity_metric_valid"]
                    )
                    page_result["baseline_fidelity_metric_exclusion_reasons"] = (
                        baseline_fidelity_classification["exclusion_reasons"]
                    )
                    if (
                        page_result["fidelity_metric_valid"]
                        and page_result["baseline_fidelity_metric_valid"]
                        and page_result.get("raw_delta_vs_baseline")
                    ):
                        page_result["delta_vs_baseline"] = dict(
                            page_result["raw_delta_vs_baseline"]
                        )
                manifest["pages"].append(page_result)
                write_results(results_path, manifest)
                metric = page_result.get("metrics", {}).get("pixels_gt_50")
                edge_bbox = page_result.get("metrics", {}).get("edge_bbox_max_delta")
                edge_row = page_result.get("metrics", {}).get("edge_row_projection_delta")
                edge_col = page_result.get("metrics", {}).get("edge_column_projection_delta")
                edge_delta = page_result.get("delta_vs_baseline", {}).get(
                    "edge_column_projection_delta"
                )
                edge_2d = page_result.get("metrics", {}).get(
                    "edge_bidirectional_mean_distance_px"
                )
                edge_2d_delta = page_result.get("delta_vs_baseline", {}).get(
                    "edge_bidirectional_mean_distance_px"
                )
                geometry_eligibility = page_result.get(
                    "geometry_verdict_eligibility"
                )
                geometry_summary = (
                    "-"
                    if geometry_eligibility is None
                    else (
                        f"{geometry_eligibility['valid_selectors']}-valid/"
                        f"{geometry_eligibility['excluded_selectors']}-excluded"
                    )
                )
                print(
                    f"{name:84} "
                    f"mode={args.capture_purpose} "
                    f"state={page_result.get('state_comparability', {}).get('classification', 'unavailable')} "
                    f"fidelity={'valid' if page_result['fidelity_metric_valid'] else 'excluded'} "
                    f"geometry={geometry_summary} "
                    f"p>50_raw={metric if metric is not None else 'capture-fail'} "
                    f"edge_bbox={edge_bbox if edge_bbox is not None else '-'} "
                    f"edge_row={edge_row if edge_row is not None else '-'} "
                    f"edge_col={edge_col if edge_col is not None else '-'} "
                    f"edge_2d={edge_2d if edge_2d is not None else '-'} "
                    f"edge_col_delta={edge_delta if edge_delta is not None else '-'} "
                    f"edge_2d_delta={edge_2d_delta if edge_2d_delta is not None else '-'}",
                    flush=True,
                )
        browser.close()

    manifest["finished_utc"] = datetime.now(timezone.utc).isoformat()
    manifest["fidelity_eligibility"] = {
        "valid_pages": sum(
            1 for page in manifest["pages"] if page["fidelity_metric_valid"]
        ),
        "excluded_pages": sum(
            1 for page in manifest["pages"] if not page["fidelity_metric_valid"]
        ),
        "excluded_page_names": [
            page["name"]
            for page in manifest["pages"]
            if not page["fidelity_metric_valid"]
        ],
        "semantics": (
            "eligibility counts only; excluded pages retain raw screenshots "
            "and metrics but do not contribute fidelity evidence"
        ),
    }
    manifest["geometry_verdict_eligibility"] = {
        "valid_selectors": sum(
            (page.get("geometry_verdict_eligibility") or {}).get(
                "valid_selectors", 0
            )
            for page in manifest["pages"]
        ),
        "excluded_selectors": sum(
            (page.get("geometry_verdict_eligibility") or {}).get(
                "excluded_selectors", 0
            )
            for page in manifest["pages"]
        ),
        "semantics": (
            "selector-level eligibility only; exclusions retain raw geometry "
            "diagnostics and do not by themselves invalidate full-page fidelity"
        ),
    }
    write_results(results_path, manifest)
    failed = [
        page["name"]
        for page in manifest["pages"]
        if not page.get("chromium", {}).get("ok") or not page.get("obscura", {}).get("ok")
    ]
    if failed:
        print(f"capture failures: {', '.join(failed)}", file=sys.stderr)
        raise SystemExit(1)
    print(f"paired captures and raw diagnostics: {results_path}")


if __name__ == "__main__":
    main()
