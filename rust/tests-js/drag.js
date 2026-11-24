// node rust/tests-js/drag.js — S-11.1 with REAL OS input (the cursor moves for ~1 s and is put back):
// dragging the header moves the window, a button in the header does not, double-click on the header maximizes,
// dragging the bottom-right edge resizes. Electron gave these natively for -webkit-app-region; Rust uses bridge.js.
const assert = require('assert'), { execFile } = require('child_process')
const { launch, until, sleep } = require('./lib')

// one PowerShell run of real mouse input: steps = [[x, y, 'down'|'up'|'move'], ...] in screen pixels
const mouse = steps => new Promise((res, rej) => execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', `
Add-Type -Namespace W -Name U -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(int f, int dx, int dy, int d, int e);
[DllImport("user32.dll")] public static extern bool GetCursorPos(out System.Drawing.Point p);' -ReferencedAssemblies System.Drawing
$p = New-Object System.Drawing.Point; [W.U]::GetCursorPos([ref]$p) | Out-Null
${steps.map(([x, y, k]) => `[W.U]::SetCursorPos(${Math.round(x)}, ${Math.round(y)}) | Out-Null; ${k === 'down' ? '[W.U]::mouse_event(2,0,0,0,0);' : k === 'up' ? '[W.U]::mouse_event(4,0,0,0,0);' : ''} Start-Sleep -Milliseconds 40`).join('\n')}
Start-Sleep -Milliseconds 300; [W.U]::SetCursorPos($p.X, $p.Y) | Out-Null`], { windowsHide: true }, e => e ? rej(e) : res()))

;(async () => {
  let a
  try {
    a = await launch({ enabled: false, autostart: false })
    await until(() => a.ui("document.readyState === 'complete' && typeof render === 'function'"), 'ui')
    await a.ctl({ cmd: 'restore' }); await a.ctl({ cmd: 'topmost', on: true }); await sleep(800)
    const at = async sel => { const i = await a.info(), r = await a.ui(`(r => [r.x + r.width / 2, r.y + r.height / 2])(document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect())`)
      return [i.outerX + r[0] * i.scale, i.y + r[1] * i.scale] }
    // a person presses, then moves: hold 250 ms (the drag starts after a page -> IPC -> host round trip)
    const drag = (from, dx, dy) => mouse([[...from, 'move'], [...from, 'down'], ...Array.from({ length: 6 }, () => [...from, 'move']), ...Array.from({ length: 10 }, (_, k) => [from[0] + dx * (k + 1) / 10, from[1] + dy * (k + 1) / 10, 'move']), [from[0] + dx, from[1] + dy, 'up']])
    let before = await a.info()
    await drag(await at('.brand-title'), 60, 40); await sleep(500)
    let after = await a.info()
    assert.ok(Math.abs(after.outerX - before.outerX - 60) <= 3 && Math.abs(after.y - before.y - 40) <= 3, `arrastar o topo move a janela: ${JSON.stringify([before.outerX, before.y, after.outerX, after.y])}`)
    console.log('ok arrastar pelo topo move a janela')
    before = after
    await drag(await at('#tabBtnLogs'), 60, 40); await sleep(500)
    after = await a.info()
    assert.ok(after.outerX === before.outerX && after.y === before.y, 'arrastar um botão (no-drag) não move a janela')
    console.log('ok botão (no-drag) não arrasta')
    const t = await at('.brand-title')
    await mouse([[...t, 'move'], [...t, 'down'], [...t, 'up'], [...t, 'down'], [...t, 'up']]); await sleep(800)
    assert.strictEqual((await a.info()).maximized, true, 'duplo clique no topo maximiza')
    const t2 = await at('.brand-title') // the header moved when the window maximized
    await mouse([[...t2, 'move'], [...t2, 'down'], [...t2, 'up'], [...t2, 'down'], [...t2, 'up']]); await sleep(800)
    assert.strictEqual((await a.info()).maximized, false, 'duplo clique de novo restaura')
    console.log('ok duplo clique no topo maximiza e restaura')
    before = await a.info()
    const corner = [before.outerX + before.w - 2, before.y + before.h - 2]
    await drag(corner, -80, -50); await sleep(600)
    after = await a.info()
    assert.ok(after.w < before.w - 40 && after.h < before.h - 20, `canto inferior direito redimensiona: ${before.w}x${before.h} -> ${after.w}x${after.h}`)
    console.log(`ok borda redimensiona (${before.w}x${before.h} -> ${after.w}x${after.h})`)
    console.log('DRAG OK')
  } catch (e) { console.error('FALHOU:', e.message); process.exitCode = 1 } finally { if (a) await a.close() }
})()
