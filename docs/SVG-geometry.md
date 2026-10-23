# SVG path length

`SVGGeometryElement.getTotalLength()` returns the computed length in local SVG
user units. It is available in render and no-render builds on paths, lines,
polylines, polygons, rectangles, circles, and ellipses.

```javascript
const path = document.querySelector('svg path');
const length = path.getTotalLength();
path.style.strokeDasharray = String(length);
path.style.strokeDashoffset = String(length);
```

Path data supports absolute and relative SVG commands, implicit repetitions,
multiple subpaths, closing segments, Bézier curves, and elliptical arcs. A syntax
error ends measurement at the last valid segment. Geometry is read afresh on
each call, including attribute and inline style changes.

Lengths do not include element transforms or the `pathLength` calibration
attribute. Basic shapes resolve absolute length units and viewport percentages
against the containing SVG viewport or its `viewBox`. Curve lengths are computed
numerically and can differ slightly from another browser's approximation.

This implementation uses presentation attributes and inline geometry declarations.
Stylesheet-cascaded geometry and animated geometry values are not implemented.
Font-relative units use the inherited inline/presentation font size; `ex` uses
half that size rather than measured font metrics.

Detached paths with explicit path data can be measured. Other detached shapes
throw `InvalidStateError` until attached to a document. Calling the method on an
incompatible receiver throws `TypeError`.

The API contract follows [SVG 2's geometry interface](https://svgwg.org/svg2-draft/types.html#InterfaceSVGGeometryElement).
