// node rust/tests-js/hover.js — the .browser beam lights while the pointer is over the Tinder page (Electron: the
// <webview> was a DOM child of .browser, so :hover applied). Real cursor moves; the cursor is put back afterwards.
const { execFile } = require('child_process'), { launch, until, sleep } = require('./lib')

const API = `Add-Type -Namespace W -Name U -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y); [DllImport("user32.dll")] public static extern bool GetCursorPos(out System.Drawing.Point p);' -ReferencedAssemblies System.Drawing`
const ps = cmd => new Promise(res => execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', `${API}\n${cmd}`], { windowsHide: true }, (e, out) => res(String(out).trim())))
const cursor = pts => ps(pts.map(([x, y]) => `[W.U]::SetCursorPos(${Math.round(x)}, ${Math.round(y)}) | Out-Null; Start-Sleep -Milliseconds 60`).join('\n'))

;(async () => {
  let a
  const saved = await ps('$p = New-Object System.Drawing.Point; [W.U]::GetCursorPos([ref]$p) | Out-Null; "$($p.X) $($p.Y)"')
  try {
    a = await launch({ enabled: false, autostart: false })
    await until(() => a.ui("document.readyState === 'complete' && !!document.querySelector('.browser > .beam')"), 'ui')
    await a.ctl({ cmd: 'restore' }); await a.ctl({ cmd: 'topmost', on: true }); await sleep(500)
    const op = () => a.ui("+getComputedStyle(document.querySelector('.browser > .beam')).opacity")
    const i = await a.info(), r = await a.ui("(r => [r.left, r.top, r.right, r.bottom])(document.getElementById('web').getBoundingClientRect())")
    const ui = [i.outerX + 100 * i.scale, i.y + 300 * i.scale], tinder = [i.outerX + (r[0] + 200) * i.scale, i.y + (r[1] + 300) * i.scale]
    await cursor([ui, [ui[0] + 150, ui[1]]])
    await until(async () => (await op()) === 0.9, 'beam at rest 0.9 with the pointer on the UI', 5000)
    await cursor([tinder])
    await until(async () => (await op()) === 1, 'beam lit while pointer is over the Tinder page', 5000)
    await cursor([ui])
    await until(async () => (await op()) === 0.9, 'beam back to rest when the pointer returns to the UI', 5000)
    console.log('HOVER OK')
  } catch (e) { console.error('FALHOU:', e.message); process.exitCode = 1 } finally {
    const [x, y] = saved.split(' ').map(Number); if (Number.isFinite(x)) await cursor([[x, y]])
    if (a) await a.close()
  }
})()
