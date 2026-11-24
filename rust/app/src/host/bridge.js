// Replaces preload.js in the UI view. Same 8 methods as `window.spark`; plus what Electron did
// natively for this page: follow the <webview id=web> placeholder, frameless dragging (-webkit-app-region)
// and edge resizing. Posts JSON through wry's window.ipc; the host answers via window.__sparkHost(msg).
(() => {
  if (window.top !== window) return
  const pending = new Map(), on = { state: [], maximized: [], focused: [] }
  let seq = 0
  const post = m => window.ipc.postMessage(JSON.stringify(m))
  Object.defineProperty(window, '__sparkHost', {
    value: m => {
      if (m.id) { const r = pending.get(m.id); pending.delete(m.id); if (r) r(m.value) }
      else for (const f of on[m.ch] || []) f(m.value)
    },
  })
  window.spark = {
    state: () => new Promise(r => { const id = ++seq; pending.set(id, r); post({ invoke: 'state', id }) }),
    onState: fn => { on.state.push(fn) },
    toggle: v => post({ ch: 'toggle', args: [v] }),
    check: () => post({ ch: 'check', args: [] }),
    win: action => post({ ch: 'win', args: [action] }),
    onMaximized: fn => { on.maximized.push(fn) },
    onFocused: fn => { on.focused.push(fn) },
    set: (k, v) => post({ ch: 'set', args: [k, v] }),
  }

  // the Tinder view sits exactly over the placeholder
  const report = () => {
    const w = document.getElementById('web')
    if (!w) return
    const r = w.getBoundingClientRect()
    post({ ch: '__bounds', args: [r.x, r.y, r.width, r.height] })
  }
  addEventListener('DOMContentLoaded', () => {
    const w = document.getElementById('web')
    if (w) new ResizeObserver(report).observe(w)
    report()
  })
  addEventListener('resize', report)

  // Electron's <webview> was a DOM child of .browser, so pointing at the Tinder page lit `.browser:hover > .beam`
  // (opacity 1 instead of 0.9). Here the page is a separate window: the UI only sees the pointer leave through
  // the placeholder's edge, so mirror that hover with a class of the same specificity.
  const hover = document.createElement('style')
  hover.textContent = '.browser.spark-hover > .beam { opacity: 1; }'
  addEventListener('DOMContentLoaded', () => document.head.append(hover))
  const browser = () => document.querySelector('.browser')
  addEventListener('mousemove', () => browser()?.classList.remove('spark-hover'), true)
  // the host knows where the real cursor is (the event's coordinates can be stale when the pointer jumps)
  document.addEventListener('mouseout', e => { if (!e.relatedTarget) post({ ch: '__leave', args: [] }) })
  on.__hover = [v => browser()?.classList.toggle('spark-hover', !!v)]

  // "Luz que segue o mouse" (spotlight, same idea as .js-luz in index.html) for the settings list and the activity
  // card. Not `.js-luz` itself: that one clips (overflow hidden) and the interval dropdown opens past the list.
  const luz = document.createElement('style')
  luz.textContent = `
    .spark-luz { position: relative; --luz-size: 220px; --luz: rgba(255, 30, 98, 0.2); }
    .spark-luz::before {
      content: ''; position: absolute; inset: 0; z-index: 0; pointer-events: none; opacity: 0;
      transition: opacity 0.3s ease; border-radius: inherit;
      background: radial-gradient(var(--luz-size) circle at var(--x, 50%) var(--y, 50%), var(--luz), transparent 70%);
    }
    .spark-luz:hover::before { opacity: 1; }
    .logs-container.spark-luz { --luz-size: 200px; --luz: rgba(255, 46, 154, 0.2); }
    .logs-container.spark-luz > .logs { position: relative; z-index: 1; }
    @media (forced-colors: active) { .spark-luz::before { display: none; } }`
  addEventListener('DOMContentLoaded', () => {
    document.head.append(luz)
    document.querySelectorAll('.settings-list, .logs-container').forEach(el => el.classList.add('spark-luz'))
  })
  addEventListener('mousemove', ev => {
    const el = ev.target.closest?.('.spark-luz')
    if (!el) return
    const r = el.getBoundingClientRect()
    el.style.setProperty('--x', (ev.clientX - r.left) + 'px')
    el.style.setProperty('--y', (ev.clientY - r.top) + 'px')
  }, true)

  // edge resize band (frameless window), then -webkit-app-region drag like Electron
  const EDGE = 6
  const edge = e => {
    const x = e.clientX, y = e.clientY, w = innerWidth, h = innerHeight
    const n = y < EDGE, s = y >= h - EDGE, wst = x < EDGE, est = x >= w - EDGE
    return (n ? 'n' : s ? 's' : '') + (wst ? 'w' : est ? 'e' : '')
  }
  const region = el => {
    for (; el && el.nodeType === 1; el = el.parentElement) {
      const cs = getComputedStyle(el), v = cs.getPropertyValue('-webkit-app-region') || cs.getPropertyValue('app-region')
      if (v === 'drag' || v === 'no-drag') return v
    }
    return 'none'
  }
  addEventListener('mousemove', e => {
    const d = edge(e)
    document.documentElement.style.cursor = d ? d + '-resize' : ''
  }, true)
  addEventListener('mousedown', e => {
    if (e.button !== 0) return
    const d = edge(e)
    if (d) { post({ ch: '__resize', args: [d] }); return }
    if (region(e.target) !== 'drag') return
    if (e.detail === 2) post({ ch: '__zoom', args: [] })
    else post({ ch: '__drag', args: [] })
  }, true)
})()
