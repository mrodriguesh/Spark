// Harness for the JS tests that ran against the Electron app (test/cursor.js, test/minimizar.js, test/reabrir.js),
// retargeted at the Rust app. The originals stay untouched; these copies change only HOW they reach the
// app: UI page over WebView2's DevTools port (was: Electron webContents), window/process over the test-hooks control
// port (was: Electron main-process --inspect). Assertions are copied verbatim.
const { spawn, execFile } = require('child_process')
const fs = require('fs'), os = require('os'), path = require('path'), net = require('net')

const ROOT = path.resolve(__dirname, '..', '..')
const EXE = process.env.SPARK_EXE || path.join(ROOT, 'rust', 'app', 'target', 'debug', 'Spark.exe')
const sleep = ms => new Promise(r => setTimeout(r, ms))
let nextPort = 9400 + Math.floor(Math.random() * 300)

async function until(fn, label, ms = 60000) {
  const end = Date.now() + ms
  while (Date.now() < end) { try { const v = await fn(); if (v) return v } catch {} await sleep(300) }
  throw new Error('Tempo esgotado: ' + label)
}

// one CDP session per call, like the original tests' main()
async function cdp(port, pick, method, params) {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json()
  const t = targets.find(pick)
  if (!t) throw new Error('no target')
  const ws = new WebSocket(t.webSocketDebuggerUrl)
  await new Promise((r, no) => { ws.onopen = r; ws.onerror = no })
  ws.send(JSON.stringify({ id: 1, method, params }))
  const d = await new Promise(r => ws.onmessage = m => { const x = JSON.parse(m.data); if (x.id === 1) r(x) })
  ws.close()
  if (d.error || d.result?.exceptionDetails) throw new Error(JSON.stringify(d.error || d.result.exceptionDetails))
  return d.result
}

// persistent CDP session (Electron's wc.debugger.attach): Emulation.* overrides live only while the session stays attached
async function openSession(port, pick) {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json()
  const t = targets.find(pick)
  if (!t) throw new Error('no target')
  const ws = new WebSocket(t.webSocketDebuggerUrl)
  await new Promise((r, no) => { ws.onopen = r; ws.onerror = no })
  let id = 0; const waiting = new Map()
  ws.onmessage = m => { const x = JSON.parse(m.data); const w = waiting.get(x.id); if (w) { waiting.delete(x.id); x.error ? w.no(new Error(JSON.stringify(x.error))) : w.ok(x.result) } }
  return { send: (method, params) => new Promise((ok, no) => { const i = ++id; waiting.set(i, { ok, no }); ws.send(JSON.stringify({ id: i, method, params })) }), close: () => ws.close() }
}

function control(port, req) {
  return new Promise((resolve, reject) => {
    const s = net.connect(port, '127.0.0.1')
    let buf = ''
    s.on('data', d => { buf += d; const i = buf.indexOf('\n'); if (i >= 0) { s.end(); resolve(JSON.parse(buf.slice(0, i))) } })
    s.on('error', reject)
    s.write(JSON.stringify(req) + '\n')
  })
}

// Electron sendInputEvent -> CDP Input.* on the UI page
function inputParams(ev) {
  if (ev.type.startsWith('mouse')) {
    const type = { mouseMove: 'mouseMoved', mouseDown: 'mousePressed', mouseUp: 'mouseReleased' }[ev.type]
    return ['Input.dispatchMouseEvent', { type, x: ev.x, y: ev.y, button: ev.button || (type === 'mouseMoved' ? 'none' : 'left'), clickCount: ev.clickCount || 0 }]
  }
  const keys = { Down: ['ArrowDown', 40], Up: ['ArrowUp', 38], Return: ['Enter', 13], Escape: ['Escape', 27], Left: ['ArrowLeft', 37], Right: ['ArrowRight', 39] }
  const [key, vk] = keys[ev.keyCode] || [ev.keyCode, 0]
  return ['Input.dispatchKeyEvent', { type: ev.type === 'keyDown' ? 'rawKeyDown' : 'keyUp', key, code: key, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk }]
}

// Launches the Rust app on a temp profile with spark.json = cfg. Returns the same helpers the originals used.
async function launch(cfg, args = [], extraEnv = {}, files = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spark-rust-'))
  fs.writeFileSync(path.join(dir, 'spark.json'), JSON.stringify(cfg))
  for (const [name, text] of Object.entries(files)) fs.writeFileSync(path.join(dir, name), text) // extra userData files before start
  const cdpPort = nextPort++, ctlPort = nextPort++
  const env = { ...process.env, SPARK_TEST_CDP_PORT: String(cdpPort), SPARK_TEST_CONTROL_PORT: String(ctlPort), SPARK_TEST_FAKE_PAGE: path.join(ROOT, 'rust', 'app', 'tests', 'fixtures', 'fake-tinder.html'), ...extraEnv }
  delete env.ELECTRON_RUN_AS_NODE
  const proc = spawn(EXE, [`--user-data-dir=${dir}`, ...args], { env })
  const isUi = t => t.type === 'page' && t.url.startsWith('http://spark.localhost/')
  const isTinder = t => t.type === 'page' && t.url.startsWith('https://tinder.com/')
  const evalIn = pick => async code => {
    const r = await cdp(cdpPort, pick, 'Runtime.evaluate', { expression: code, returnByValue: true, awaitPromise: true })
    return r.result.value
  }
  const ui = evalIn(isUi), tinder = evalIn(isTinder)
  const input = ev => { const [m, p] = inputParams(ev); return cdp(cdpPort, isUi, m, p) }
  const ctl = req => control(ctlPort, req)
  const info = async () => { const s = await ctl({ cmd: 'info' }); return { min: s.min, visivel: s.visible, x: s.x, ...s } }
  const close = async () => {
    try { proc.kill() } catch {}
    await killProfile(dir); await sleep(1500)
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 })
  }
  await until(() => ctl({ cmd: 'info' }).then(s => s.pid), 'control port', 30000).catch(() => {})
  // like the originals' fixed wait after spawn: return once the UI page has loaded
  await until(() => ui("document.readyState === 'complete' && typeof render === 'function'"), 'UI page', 60000).catch(() => {})
  const session = () => openSession(cdpPort, isUi)
  return { ui, tinder, input, ctl, info, close, session, dir, proc, cdpPort, ctlPort }
}

const killProfile = dir => new Promise(res => execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
  `Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.CommandLine.IndexOf(${"'" + dir.replace(/'/g, "''") + "'"}, [StringComparison]::OrdinalIgnoreCase) -ge 0 } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }`],
  { windowsHide: true }, () => res()))

// every process of the app: the exe and the WebView2 tree under it
// The app's processes: Spark.exe and its WebView2 (msedgewebview2.exe) tree — what Electron's app.getAppMetrics()
// listed. Helper children such as the geolocation/Task Scheduler PowerShell are not app processes there either
// (Node's execFile children may outlive the app by their own timeout), so they are left out.
function processTree(rootPid) {
  return new Promise(res => execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
    `$all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name; $ids = @(${rootPid}); $i = 0
     while ($i -lt $ids.Count) { $ids += @($all | Where-Object { $_.ParentProcessId -eq $ids[$i] -and $_.Name -in @('Spark.exe', 'msedgewebview2.exe') } | ForEach-Object { $_.ProcessId }); $i++ }; $ids -join ','`],
    { windowsHide: true }, (e, out) => res(String(out).trim().split(',').filter(Boolean).map(Number))))
}

// WM_CLOSE to the app's main window, like the user's native close (Alt+F4 / taskbar) — what Electron's win.close() stood for
function nativeClose(pid) {
  return new Promise(res => execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
    `$p = Get-Process -Id ${pid} -ErrorAction SilentlyContinue; if ($p) { $p.Refresh(); if ($p.MainWindowHandle -ne 0) { $p.CloseMainWindow() } else { 'nowindow' } }`],
    { windowsHide: true }, (e, out) => res(String(out).trim())))
}

module.exports = { launch, until, sleep, processTree, killProfile, nativeClose, ROOT, EXE }
