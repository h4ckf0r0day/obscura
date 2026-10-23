use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

fn runtime() -> ObscuraJsRuntime {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html(
        r#"<svg id="svg" width="200" height="100" viewBox="0 0 200 100"></svg>"#,
    ));
    rt.run_page_init();
    rt.evaluate(
        r#"globalThis.shape = (tag, attrs) => {
        const element = document.createElementNS('http://www.w3.org/2000/svg', tag);
        for (const [name, value] of Object.entries(attrs)) element.setAttribute(name, value);
        document.getElementById('svg').appendChild(element);
        return element;
    };"#,
    )
    .unwrap();
    rt
}

#[test]
fn svg_total_length_handles_path_commands_and_subpaths() {
    let mut rt = runtime();
    let lengths = rt
        .evaluate(
            r#"[
        'M0 0L3 4H6V0Z',
        'm1 1 3 4 3 -4z',
        'M0 0C0 100 100 100 100 0',
        'M0 0Q50 100 100 0',
        'M0 0A100 100 0 0 1 200 0',
        'M100 100M3 4L6 8',
        'M0 0L3 4zM10 10l3 4z',
        'M0 0L3 4 rubbish',
        ''
    ].map(d => shape('path', {d}).getTotalLength())"#,
        )
        .unwrap();
    let expected = [
        18.0,
        16.0,
        200.0,
        147.894285754,
        std::f64::consts::PI * 100.0,
        5.0,
        20.0,
        5.0,
        0.0,
    ];
    for (value, expected) in lengths.as_array().unwrap().iter().zip(expected) {
        assert!(
            (value.as_f64().unwrap() - expected).abs() < 0.01,
            "{value} != {expected}"
        );
    }
    assert_eq!(rt.evaluate(r#"(() => {
        const length = d => shape('path', {d}).getTotalLength();
        return [
            length('M0 0C10 20 20 20 30 0S50 -20 60 0') === length('M0 0C10 20 20 20 30 0C40 -20 50 -20 60 0'),
            length('M0 0Q10 20 20 0T40 0') === length('M0 0Q10 20 20 0Q30 -20 40 0'),
            length('M0 0a0 10 0 0 1 3 4') === 5
        ];
    })()"#).unwrap(), json!([true, true, true]));
}

#[test]
fn svg_total_length_handles_basic_shapes_and_user_units() {
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"[
        shape('line', {x1:1, y1:1, x2:4, y2:5}).getTotalLength(),
        shape('polyline', {points:'0,0 3,4 6,0'}).getTotalLength(),
        shape('polygon', {points:'0,0 3,4 6,0'}).getTotalLength(),
        shape('polygon', {points:'0,0 3,4 6'}).getTotalLength(),
        shape('rect', {width:10, height:20}).getTotalLength(),
        shape('rect', {width:0, height:10}).getTotalLength(),
        shape('circle', {r:100}).getTotalLength(),
        shape('ellipse', {rx:100, ry:50}).getTotalLength(),
        shape('ellipse', {rx:10, ry:0}).getTotalLength(),
        shape('rect', {width:100, height:80, rx:20}).getTotalLength(),
        shape('line', {x2:'1in'}).getTotalLength(),
        shape('line', {x2:'50%'}).getTotalLength(),
        shape('circle', {r:-1}).getTotalLength(),
        shape('path', {d:'M0 0L3 4', pathLength:1, transform:'scale(10)'}).getTotalLength()
    ]"#,
        )
        .unwrap();
    let expected = [
        5.0,
        10.0,
        16.0,
        10.0,
        60.0,
        20.0,
        std::f64::consts::TAU * 100.0,
        484.422411027,
        40.0,
        200.0 + std::f64::consts::TAU * 20.0,
        96.0,
        100.0,
        0.0,
        5.0,
    ];
    for (value, expected) in result.as_array().unwrap().iter().zip(expected) {
        assert!(
            (value.as_f64().unwrap() - expected).abs() < 0.001,
            "{value} != {expected}"
        );
    }
}

#[test]
fn svg_total_length_updates_geometry_and_supports_stroke_animation_setup() {
    let mut rt = runtime();
    assert_eq!(
        rt.evaluate(
            r#"(() => {
        const path = shape('path', {d:'M0 0L3 4'});
        const before = path.getTotalLength();
        path.setAttribute('d', 'M0 0L6 8');
        const after = path.getTotalLength();
        path.style.setProperty('d', 'path("M0 0L9 12")');
        const styled = path.getTotalLength();
        path.style.strokeDasharray = String(styled);
        path.style.strokeDashoffset = String(styled);
        const line = shape('line', {x2:'50%'});
        const firstViewport = line.getTotalLength();
        document.getElementById('svg').setAttribute('viewBox', '0 0 400 200');
        const secondViewport = line.getTotalLength();
        const rect = shape('rect', {width:10, height:20, style:'width:30px;height:40px'});
        return [before, after, styled, path.style.strokeDasharray,
            path.style.strokeDashoffset, firstViewport, secondViewport, rect.getTotalLength(),
            typeof SVGGeometryElement.prototype.getTotalLength];
    })()"#
        )
        .unwrap(),
        json!([5, 10, 15, "15", "15", 100, 200, 140, "function"])
    );
}

#[test]
fn svg_total_length_preserves_receiver_and_detached_element_behavior() {
    let mut rt = runtime();
    assert_eq!(rt.evaluate(r#"(() => {
        const ns = 'http://www.w3.org/2000/svg';
        const path = document.createElementNS(ns, 'path');
        path.setAttribute('d', 'M0 0L3 4');
        const line = document.createElementNS(ns, 'line');
        line.setAttribute('x2', 3); line.setAttribute('y2', 4);
        const errors = [];
        try { SVGGeometryElement.prototype.getTotalLength.call({}); } catch (e) { errors.push(e.name); }
        try { line.getTotalLength(); } catch (e) { errors.push(e.name); }
        document.getElementById('svg').appendChild(line);
        line.style.display = 'none';
        return [path.getTotalLength(), errors, line.getTotalLength(),
            typeof document.createElement('div').getTotalLength];
    })()"#).unwrap(), json!([5, ["TypeError", "InvalidStateError"], 5, "undefined"]));
}
