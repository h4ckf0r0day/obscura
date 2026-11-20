#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"

/**
 * Cookie Consent Dismissal Helper
 *
 * Automatically accepts cookie consent dialogs on EU websites.
 * Supports common CMPs: OneTrust, Cookiebot, Didomi, Quantcast, Google, BBC, Amazon, etc.
 *
 * Usage:
 *   ./dismiss-cookies.js          # Accept cookies
 *   ./dismiss-cookies.js --reject # Reject cookies (where possible)
 */

import { cdpCall, ensureBroker, evaluate, sessionAdapter } from "./broker-client.js";
import { applyActiveEmulation } from "./emulation-state.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

const { has, get } = parseFlags(process.argv.slice(2), { values: ["--tab", "--wait"] });
const reject = has("--reject");
const mode = reject ? "reject" : "accept";

const COOKIE_DISMISS_SCRIPT = `(acceptCookies) => {
  const clicked = [];

  const isVisible = (el) => {
    if (!el) return false;
    const style = getComputedStyle(el);
    return style.display !== 'none' && 
           style.visibility !== 'hidden' && 
           style.opacity !== '0' &&
           (el.offsetParent !== null || style.position === 'fixed' || style.position === 'sticky');
  };

  const tryClick = (selector, description) => {
    const el = typeof selector === 'string' ? document.querySelector(selector) : selector;
    if (isVisible(el)) {
      el.click();
      clicked.push(description || selector);
      return true;
    }
    return false;
  };

  const findButtonByText = (patterns, container = document) => {
    const buttons = Array.from(container.querySelectorAll('button, [role="button"], a.button, input[type="submit"], input[type="button"]'));
    // Sort patterns by length descending to match more specific patterns first
    const sortedPatterns = [...patterns].sort((a, b) => b.length - a.length);
    
    // Check patterns in order of specificity (longest first)
    for (const pattern of sortedPatterns) {
      for (const btn of buttons) {
        const text = (btn.textContent || btn.value || '').trim().toLowerCase();
        if (text.length > 100) continue; // Skip buttons with very long text
        if (!isVisible(btn)) continue; // Skip hidden buttons
        if (typeof pattern === 'string' ? text.includes(pattern) : pattern.test(text)) {
          return btn;
        }
      }
    }
    return null;
  };
  
  const acceptPatterns = [
    'accept all', 'accept cookies', 'allow all', 'allow cookies',
    'i agree', 'i accept', 'yes, i agree', 'agree and continue',
    'alle akzeptieren', 'akzeptieren', 'alle zulassen', 'zustimmen',
    'annehmen', 'einverstanden',
    'accepter tout', 'tout accepter', "j'accepte", 'accepter et continuer', 'accepter',
    'accetta tutti', 'accetta', 'accetto',
    'aceptar todo', 'aceptar', 'acepto',
    'aceitar tudo', 'aceitar',
    'continue', 'agree',
  ];

  const rejectPatterns = [
    'reject all', 'decline all', 'deny all', 'refuse all',
    'i do not agree', 'i disagree', 'no thanks',
    'alle ablehnen', 'ablehnen', 'nicht zustimmen',
    'refuser tout', 'tout refuser', 'refuser',
    'rifiuta tutti', 'rifiuta',
    'rechazar todo', 'rechazar',
    'rejeitar tudo', 'rejeitar',
    'only necessary', 'necessary only', 'nur notwendige',
    'essential only', 'nur essentielle',
  ];

  const patterns = acceptCookies ? acceptPatterns : rejectPatterns;

  // OneTrust
  if (document.querySelector('#onetrust-banner-sdk')) {
    const selector = acceptCookies ? '#onetrust-accept-btn-handler' : '#onetrust-reject-all-handler';
    if (tryClick(selector, 'OneTrust')) return clicked;
  }

  // Google
  if (document.querySelector('[data-consent-dialog]') || document.querySelector('form[action*="consent.google"]') || document.querySelector('#CXQnmb')) {
    const selector = acceptCookies ? '#L2AGLb' : '#W0wltc';
    if (tryClick(selector, 'Google Consent')) return clicked;
  }

  // YouTube (Google-owned, custom consent element)
  if (document.querySelector('ytd-consent-bump-v2-lightbox')) {
    const btn = Array.from(document.querySelectorAll('ytd-consent-bump-v2-lightbox button'))
      .find(b => acceptCookies 
        ? b.textContent.includes('Accept all') || b.ariaLabel?.includes('Accept')
        : b.textContent.includes('Reject all') || b.ariaLabel?.includes('Reject'));
    if (btn) {
      btn.click();
      clicked.push('YouTube');
      return clicked;
    }
  }

  // Cookiebot
  if (document.querySelector('#CybotCookiebotDialog')) {
    const selector = acceptCookies
      ? '#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll, #CybotCookiebotDialogBodyButtonAccept'
      : '#CybotCookiebotDialogBodyButtonDecline, #CybotCookiebotDialogBodyLevelButtonLevelOptinDeclineAll';
    if (tryClick(selector, 'Cookiebot')) return clicked;
  }

  // Didomi
  if (document.querySelector('#didomi-host') || window.Didomi) {
    const selector = acceptCookies ? '#didomi-notice-agree-button' : '#didomi-notice-disagree-button, [data-testid="disagree-button"]';
    if (tryClick(selector, 'Didomi')) return clicked;
  }

  // Quantcast
  if (document.querySelector('.qc-cmp2-container')) {
    const selector = acceptCookies
      ? '.qc-cmp2-summary-buttons button[mode="primary"], .qc-cmp2-button[data-testid="accept-all"]'
      : '.qc-cmp2-summary-buttons button[mode="secondary"], .qc-cmp2-button[data-testid="reject-all"]';
    if (tryClick(selector, 'Quantcast')) return clicked;
  }

  // Usercentrics (shadow DOM)
  const ucRoot = document.querySelector('#usercentrics-root');
  if (ucRoot && ucRoot.shadowRoot) {
    const shadow = ucRoot.shadowRoot;
    const btn = acceptCookies
      ? shadow.querySelector('[data-testid="uc-accept-all-button"]')
      : shadow.querySelector('[data-testid="uc-deny-all-button"]');
    if (btn) { btn.click(); clicked.push('Usercentrics'); return clicked; }
  }

  // TrustArc
  if (document.querySelector('#truste-consent-track') || document.querySelector('.trustarc-banner')) {
    const selector = acceptCookies ? '#truste-consent-button, .trustarc-agree-btn' : '.trustarc-decline-btn';
    if (tryClick(selector, 'TrustArc')) return clicked;
  }

  // Klaro
  if (document.querySelector('.klaro')) {
    const selector = acceptCookies ? '.klaro .cm-btn-accept-all, .klaro .cm-btn-success' : '.klaro .cm-btn-decline';
    if (tryClick(selector, 'Klaro')) return clicked;
  }

  // BBC
  if (document.querySelector('#bbccookies, .bbccookies-banner')) {
    if (acceptCookies && tryClick('#bbccookies-continue-button', 'BBC')) return clicked;
  }

  // Amazon
  if (document.querySelector('#sp-cc') || document.querySelector('#sp-cc-accept')) {
    const selector = acceptCookies ? '#sp-cc-accept' : '#sp-cc-rejectall-link, #sp-cc-decline';
    if (tryClick(selector, 'Amazon')) return clicked;
  }

  // CookieYes
  if (document.querySelector('#cookie-law-info-bar') || document.querySelector('.cky-consent-container')) {
    const selector = acceptCookies ? '#cookie_action_close_header, .cky-btn-accept' : '.cky-btn-reject';
    if (tryClick(selector, 'CookieYes')) return clicked;
  }

  // Generic containers
  const consentContainers = [
    '[class*="cookie-banner"]', '[class*="cookie-consent"]', '[class*="cookie-notice"]',
    '[class*="cookieBanner"]', '[class*="cookieConsent"]', '[class*="cookieNotice"]',
    '[id*="cookie-banner"]', '[id*="cookie-consent"]', '[id*="cookie-notice"]',
    '[class*="consent-banner"]', '[class*="consent-modal"]', '[class*="consent-dialog"]',
    '[class*="gdpr"]', '[id*="gdpr"]', '[class*="privacy-banner"]', '[class*="privacy-notice"]',
    '[role="dialog"][aria-label*="cookie" i]', '[role="dialog"][aria-label*="consent" i]',
  ];

  for (const containerSel of consentContainers) {
    const containers = document.querySelectorAll(containerSel);
    for (const container of containers) {
      if (!isVisible(container)) continue;
      // Skip html/body elements that might match
      if (container.tagName === 'HTML' || container.tagName === 'BODY') continue;
      const btn = findButtonByText(patterns, container);
      if (btn) { btn.click(); clicked.push('Generic (' + containerSel + ')'); return clicked; }
    }
  }

  // Last resort: find button near cookie-related text content
  // Look for visible containers that mention "cookie" and have accept/reject buttons
  // Include custom elements (Reddit uses rpl-modal-card, etc.)
  const allContainers = document.querySelectorAll('div, section, aside, [class*="modal"], [class*="dialog"], [role="dialog"]');
  for (const container of allContainers) {
    if (!isVisible(container)) continue;
    const text = container.textContent?.toLowerCase() || '';
    // Must mention cookies and be reasonably sized (not the whole page)
    if (text.includes('cookie') && text.length > 100 && text.length < 3000) {
      const btn = findButtonByText(patterns, container);
      if (btn && isVisible(btn)) {
        btn.click();
        clicked.push('Generic (text-based)');
        return clicked;
      }
    }
  }

  // Final fallback: look for any visible button with exact accept/reject text
  // that appears alongside cookie-related content on the page
  if (document.body.textContent?.toLowerCase().includes('cookie')) {
    const exactPatterns = acceptCookies 
      ? ['accept all', 'accept cookies', 'allow all', 'i agree', 'alle akzeptieren']
      : ['reject all', 'decline all', 'reject optional', 'alle ablehnen'];
    const singleWordPatterns = acceptCookies ? ['accept', 'agree'] : ['reject', 'decline'];
    const buttons = document.querySelectorAll('button, [role="button"]');
    for (const btn of buttons) {
      if (!isVisible(btn)) continue;
      const text = (btn.textContent || '').trim().toLowerCase();
      if (exactPatterns.some(p => text.includes(p))) {
        btn.click();
        clicked.push('Generic (exact match)');
        return clicked;
      }
    }
    // Try single-word matches as last resort
    for (const btn of buttons) {
      if (!isVisible(btn)) continue;
      const text = (btn.textContent || '').trim().toLowerCase();
      if (singleWordPatterns.some(p => text === p)) {
        btn.click();
        clicked.push('Generic (single word)');
        return clicked;
      }
    }
  }

  return clicked;
}`;

const IFRAME_DISMISS_SCRIPT = `(acceptCookies) => {
  const clicked = [];

  const isVisible = (el) => {
    if (!el) return false;
    const style = getComputedStyle(el);
    return style.display !== 'none' && 
           style.visibility !== 'hidden' && 
           style.opacity !== '0' &&
           (el.offsetParent !== null || style.position === 'fixed' || style.position === 'sticky');
  };

  const rejectIndicators = ['do not', "don't", 'nicht', 'no ', 'refuse', 'reject', 'decline', 'deny', 'disagree', 'ablehnen', 'refuser', 'rifiuta', 'rechazar', 'manage', 'settings', 'options', 'customize'];
  const acceptIndicators = ['accept', 'agree', 'allow', 'yes', 'ok', 'got it', 'continue', 'akzeptieren', 'zustimmen', 'accepter', 'accetta', 'aceptar'];

  const isRejectButton = (text) => rejectIndicators.some(p => text.includes(p));
  const isAcceptButton = (text) => acceptIndicators.some(p => text.includes(p)) && !isRejectButton(text);

  const buttons = document.querySelectorAll('button, [role="button"]');
  for (const btn of buttons) {
    const text = (btn.textContent || '').trim().toLowerCase();
    if (!isVisible(btn)) continue;
    const shouldClick = acceptCookies ? isAcceptButton(text) : isRejectButton(text);
    if (shouldClick) { btn.click(); clicked.push('iframe: ' + text.slice(0, 30)); return clicked; }
  }

  const spBtn = acceptCookies
    ? document.querySelector('[title="Accept All"], [title="Accept"], [aria-label*="Accept"]')
    : document.querySelector('[title="Reject All"], [title="Reject"], [aria-label*="Reject"]');
  if (spBtn) { spBtn.click(); clicked.push('Sourcepoint iframe'); return clicked; }

  return clicked;
}`;

// Recursively collect all frames
function collectFrames(frameTree, frames = []) {
  frames.push({ id: frameTree.frame.id, url: frameTree.frame.url });
  if (frameTree.childFrames) {
    for (const child of frameTree.childFrames) {
      collectFrames(child, frames);
    }
  }
  return frames;
}

// Global timeout
const globalTimeout = setTimeout(() => {
  console.error("✗ Global timeout exceeded (30s)");
  process.exit(1);
}, 30000);

try {
  log("connecting...");
  await ensureBroker();
  log("broker ready");
  const tabId = get("--tab", undefined);
  await applyActiveEmulation(sessionAdapter(tabId), null);

  // Wait for consent dialogs to appear (some CMPs inject after load).
  const waitMs = Number(get("--wait", 500)) || 500;
  await new Promise((r) => setTimeout(r, waitMs));

  log("trying main page...");
  let result = await evaluate(`(${COOKIE_DISMISS_SCRIPT})(${!reject})`, { tabId, timeout: 30000 });

  // If nothing found, try iframes
  if (result.length === 0) {
    log("trying iframes...");
    try {
      const frameTree = (await cdpCall("Page.getFrameTree", {}, { tabId })).frameTree;
      const frames = collectFrames(frameTree);

      for (const frame of frames) {
        if (frame.url === "about:blank" || frame.url.startsWith("javascript:")) continue;
        if (
          frame.url.includes("sp_message") ||
          frame.url.includes("consent") ||
          frame.url.includes("privacy") ||
          frame.url.includes("cmp") ||
          frame.url.includes("sourcepoint") ||
          frame.url.includes("cookie") ||
          frame.url.includes("privacy-mgmt")
        ) {
          log("trying frame:", frame.url.slice(0, 60));
          try {
            const { executionContextId } = await cdpCall(
              "Page.createIsolatedWorld",
              { frameId: frame.id, worldName: "cookie-dismiss" },
              { tabId },
            );
            const frameResult = (
              await cdpCall(
                "Runtime.evaluate",
                {
                  expression: `(${IFRAME_DISMISS_SCRIPT})(${!reject})`,
                  contextId: executionContextId,
                  returnByValue: true,
                  awaitPromise: true,
                },
                { tabId, timeout: 30000 },
              )
            ).result?.value ?? [];
            if (frameResult.length > 0) {
              result = frameResult;
              break;
            }
          } catch (e) {
            log("frame error:", e.message);
          }
        }
      }
    } catch (e) {
      log("getFrameTree error:", e.message);
    }
  }

  if (result.length > 0) {
    console.log(`✓ Dismissed cookie dialog (${mode}): ${result.join(", ")}`);
  } else {
    console.log(`○ No cookie dialog found to ${mode}`);
  }

  log("done");
} catch (e) {
  console.error("✗", e.message);
  process.exit(1);
} finally {
  clearTimeout(globalTimeout);
  setTimeout(() => process.exit(0), 100);
}
