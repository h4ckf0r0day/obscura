# Autonomous AI-Agent Workflows & Semantic DOM Engine

## 1. The Challenge of Web Browsing for AI Agents

Modern web pages are filled with boilerplate DOM elements, script tags, tracking pixels, nested divs, and SVG icons. Sending raw HTML to an LLM context window:
1. Consumes 50,000+ tokens per page.
2. Distracts the model with non-semantic noise.
3. Slows inference latency and dramatically inflates API costs.
4. Makes selector targeting brittle and prone to hallucination.

---

## 2. Obscura Next Agent Architecture

Obscura Next introduces a dedicated AI agent layer (`obscura-agent`) that provides:

```
                  Raw Web Page / DOM Tree
                             │
                             ▼
              [Semantic DOM Distiller]
         - Strips scripts, styles, invisibles
         - Computes accessibility roles & labels
         - Assigns numeric Element Reference IDs (@e1, @e2...)
                             │
                             ▼
              [Compact Semantic Snapshot]
           (Markdown / Structured JSON Representation)
                     ~90% Token Reduction!
                             │
                             ▼
                 [AI Agent Decision Loop]
          (LLM outputs action: {"action":"click", "target":"@e4"})
                             │
                             ▼
              [Action Protocol Executor]
         - Resolves target coordinates
         - Dispatches synthetic events (click/input/key)
         - Waits for DOM quiescence & network idle
```

---

## 3. High-Level Agent Actions

The action protocol supports the following atomic agent operations:

- `navigate(url, wait_until)`: Navigate to target URL and wait for page settle.
- `click(target)`: Click interactive element identified by selector or `@eID`.
- `type(target, text, clear_first)`: Input text into field with realistic keypress simulation.
- `select(target, value)`: Select option from dropdown menu.
- `scroll(direction, pixels)`: Scroll viewport or specific element.
- `extract_semantic_tree()`: Return simplified accessibility/interactive tree for AI reasoning.
- `screenshot(path, full_page)`: Capture visual verification screenshot.
- `wait_for_selector(selector, timeout_ms)`: Wait for element to appear in DOM.
- `wait_for_idle(settle_ms)`: Wait for network requests and DOM mutations to stabilize.

