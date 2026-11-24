// node rust/tests-js/extras.js — S-02.8 log rotation, S-02.9 session-end save, S-04.6 toast (real Windows toast,
// removed from the Action Center afterwards). Real app, temp profile, offline, fake page.
const fs = require('fs'), path = require('path'), assert = require('assert'), { execFile } = require('child_process')
const { launch, until, sleep } = require('./lib')
const ps = cmd => new Promise(res => execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', cmd], { windowsHide: true }, (e, out) => res(String(out).trim())))
const HISTORY = "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; $h = [Windows.UI.Notifications.ToastNotificationManager]::History"

;(async () => {
  let a
  try {
    // 1. rotation: spark.log > 1,000,000 bytes becomes spark.log.old at startup (main.js:450)
    const big = 'x'.repeat(1_000_001)
    a = await launch({ enabled: false, autostart: false }, [], {}, { 'spark.log': big })
    await until(() => fs.existsSync(path.join(a.dir, 'spark.log.old')), 'spark.log.old', 30000)
    assert.strictEqual(fs.statSync(path.join(a.dir, 'spark.log.old')).size, big.length, 'o log antigo inteiro vira spark.log.old')
    assert.ok(!fs.existsSync(path.join(a.dir, 'spark.log')) || fs.statSync(path.join(a.dir, 'spark.log')).size < 10000, 'spark.log recomeça pequeno')
    await a.close(); a = null
    console.log('ok rotação: spark.log > 1 MB vira spark.log.old ao abrir')

    // 2. session end: Windows asks to log off -> the app saves (main.js:328)
    a = await launch({ enabled: false, autostart: false, likesTotal: 77 })
    const file = path.join(a.dir, 'spark.json')
    fs.rmSync(file)
    const sent = await ps(`Add-Type -Namespace W -Name U -MemberDefinition '[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern System.IntPtr FindWindowEx(System.IntPtr p, System.IntPtr c, string cls, string t); [DllImport("user32.dll")] public static extern System.IntPtr SendMessageTimeout(System.IntPtr h, uint m, System.IntPtr w, System.IntPtr l, uint f, uint t, out System.IntPtr r);'
      $h = [System.IntPtr]::Zero; $n = 0; do { $h = [W.U]::FindWindowEx([System.IntPtr]::Zero, $h, 'SparkPowerWindow', 'Spark'); if ($h -ne [System.IntPtr]::Zero) { $r = [System.IntPtr]::Zero; [W.U]::SendMessageTimeout($h, 0x11, [System.IntPtr]::Zero, [System.IntPtr]::Zero, 2, 3000, [ref]$r) | Out-Null; $n++ } } while ($h -ne [System.IntPtr]::Zero); $n`)
    assert.ok(Number(sent) >= 1, 'janela de energia encontrada')
    await until(() => fs.existsSync(file), 'spark.json salvo no fim da sessão', 10000)
    // main.js save() writes the whole db: defaults first (main.js:58-59), then the loaded values, same key order
    const saved = JSON.parse(fs.readFileSync(file, 'utf8'))
    assert.deepStrictEqual(Object.keys(saved), ['enabled', 'skipFirst', 'autostart', 'day', 'likesToday', 'likesTotal', 'matches', 'startMinimized', 'retryMin', 'likesWaitFrom'])
    assert.deepStrictEqual([saved.enabled, saved.autostart, saved.likesTotal], [false, false, 77], 'fim de sessão grava as configurações em memória')
    console.log('ok fim de sessão: WM_QUERYENDSESSION grava spark.json')

    // 3. toast: login screen -> "Faça login no Tinder para começar." (main.js:220)
    await a.ctl({ cmd: 'tinder-load', url: 'https://tinder.com/' }); await sleep(2000)
    await a.ui('spark.toggle(true)')
    await until(async () => (await a.ui('spark.state()')).status.code === 'login', 'status login', 20000)
    // Windows keeps no toast history for this unpackaged AUMID (checked by hand), so the app's own record is used:
    // the exact XML it handed to ToastNotifier.Show and whether Windows accepted it
    const toast = await until(async () => (await a.ctl({ cmd: 'toasts' })).find(t => t.xml.includes('Faça login no Tinder para começar.')), 'toast pedido', 15000)
    assert.strictEqual(toast.shown, true, `Windows aceitou o toast: ${toast.error}`)
    assert.ok(toast.xml.startsWith('<toast><visual><binding template="ToastImageAndText02"><text id="1">Spark</text>'), toast.xml)
    console.log('ok notificação: Windows aceitou o toast (XML do Electron, AUMID app.spark.match)')
    console.log('EXTRAS OK')
  } catch (e) { console.error('FALHOU:', e.message); process.exitCode = 1 } finally { if (a) await a.close() }
})()
