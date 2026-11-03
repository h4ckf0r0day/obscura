// Obscura Next Dashboard Logic & Real-time Canvas Rendering

function drawChart(canvasId, data, color) {
  const canvas = document.getElementById(canvasId);
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  const w = canvas.width;
  const h = canvas.height;

  ctx.clearRect(0, 0, w, h);

  // Grid lines
  ctx.strokeStyle = '#30363d';
  ctx.lineWidth = 1;
  for (let y = 30; y < h; y += 40) {
    ctx.beginPath();
    ctx.moveTo(30, y);
    ctx.lineTo(w - 10, y);
    ctx.stroke();
  }

  // Draw Line
  ctx.strokeStyle = color;
  ctx.lineWidth = 2.5;
  ctx.beginPath();

  const max = Math.max(...data) * 1.2 || 1;
  const stepX = (w - 50) / (data.length - 1);

  data.forEach((val, idx) => {
    const x = 35 + idx * stepX;
    const y = h - 25 - (val / max) * (h - 50);
    if (idx === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });
  ctx.stroke();

  // Draw points
  ctx.fillStyle = color;
  data.forEach((val, idx) => {
    const x = 35 + idx * stepX;
    const y = h - 25 - (val / max) * (h - 50);
    ctx.beginPath();
    ctx.arc(x, y, 4, 0, Math.PI * 2);
    ctx.fill();
  });
}

const latencySamples = [0.08, 0.07, 0.09, 0.06, 0.08, 0.05, 0.07, 0.06];
const memorySamples = [112, 118, 120, 124, 122, 126, 128, 127];

window.addEventListener('load', () => {
  drawChart('latencyChart', latencySamples, '#58a6ff');
  drawChart('memoryChart', memorySamples, '#3fb950');
});

function recycleWorker(id) {
  alert(`Worker ${id} flagged for zero-downtime drain and recycle.`);
}

function executeAgentAction(type) {
  const url = document.getElementById('action-url').value;
  const out = document.getElementById('action-result');

  if (type === 'navigate') {
    out.textContent = JSON.stringify({
      action: "navigate",
      status: "success",
      url: url,
      title: "Example Domain",
      elapsed_ms: 54.2,
      http_status: 200,
      protocol: "HTTP/2",
      security_perimeter: "validated_public_ip"
    }, null, 2);
  } else if (type === 'extract_tree') {
    out.textContent = JSON.stringify({
      action: "extract_semantic_tree",
      status: "success",
      tree: {
        id: "@e1",
        tag: "html",
        children: [
          { id: "@e2", tag: "h1", text: "Example Domain" },
          { id: "@e3", tag: "p", text: "This domain is for illustrative examples in documents." },
          { id: "@e4", tag: "a", text: "More information...", selector: "a[href*='iana.org']", is_interactive: true }
        ]
      },
      token_reduction: "89.4%"
    }, null, 2);
  } else if (type === 'extract_markdown') {
    out.textContent = `# Example Domain\n\nThis domain is for illustrative examples in documents.\n\n- [@e4 (a)] "More information..." -> a[href*='iana.org']`;
  } else if (type === 'simulate_ssrf') {
    out.textContent = JSON.stringify({
      action: "validate_url",
      target: "http://169.254.169.254/latest/meta-data",
      blocked: true,
      reason: "SSRF policy blocked request to private/restricted address: 169.254.169.254",
      defense: "obscura-ssrf IP classification gate"
    }, null, 2);
  }
}
