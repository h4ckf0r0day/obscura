window.__jsHeavyPageInvoked.push('post-busy-loop');
setInterval(function jsHeavyPageTick() {
  window.__jsHeavyPageTicks += 1;
}, 0);
