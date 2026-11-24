// node rust/tests-js/minimizar.js — copy of test/minimizar.js retargeted at the Rust app. Same assertions and messages;
// only the way the app is reached changed (rust/tests-js/lib.js). Mapping of the Electron main-process calls:
//   win.isMinimized()/isVisible()/getPosition()[0]  -> ctl({ cmd: 'info' })  { min, visible, x }
//   win.minimize() / win.restore(); win.focus()      -> ctl({ cmd: 'minimize' }) / ctl({ cmd: 'restore' })
//   win.setSize(w, h)                                -> ctl({ cmd: 'size', w, h })
//   app.emit('second-instance')                      -> ctl({ cmd: 'second-instance' })
//   win.close() ('native')                           -> WM_CLOSE to the main window (lib.nativeClose); fallback ctl({ cmd: 'close' })
//   webContents.executeJavaScript(code)              -> ui(code) (Runtime.evaluate in the UI page over WebView2's DevTools port)
//   app.getAppMetrics().map(p => p.pid)              -> processTree(proc.pid): the exe and every WebView2 process under it
//   session.protocol.handle('https', fixture) + wc.loadURL(...) -> SPARK_TEST_FAKE_PAGE (fixture file) at launch + ctl({ cmd: 'tinder-load' })
//   win.capturePage()                                -> ctl({ cmd: 'capture', path })  (whole client area; no crop, see below)
// The test-hooks build forces an invalid proxy, so '--proxy-server=http://127.0.0.1:9' maps to launching WITHOUT the fake page
// (SPARK_TEST_FAKE_PAGE unset => https://tinder.com/* fails to connect, i.e. "offline"); launches that had no proxy argument keep the fake page.
// '--hidden' is the same argument of the Rust exe.
const assert = require('assert')
const fs = require('fs'), os = require('os'), path = require('path')
const { launch: rawLaunch, sleep, processTree, nativeClose, ROOT } = require('./lib')

const RESULTS = path.join(__dirname, 'results', 'minimizar')
fs.mkdirSync(RESULTS, { recursive: true })

// abre o Spark com essas configurações e argumentos; devolve { info(), ui(), ctl(), dir, close(), proc }
// offline = the original passed --proxy-server=http://127.0.0.1:9 (no network): no fake page for tinder.com
async function launch(cfg, args = [], { offline = false, fakePage } = {}) {
  const env = {}
  if (offline) env.SPARK_TEST_FAKE_PAGE = undefined
  else if (fakePage) env.SPARK_TEST_FAKE_PAGE = fakePage
  const a = await rawLaunch({ enabled: false, autostart: false, ...cfg }, args, env)
  await sleep(8000)
  return a
}

// Windows reuses pids quickly: print name, parent and start time of every pid of the tree that still answers, so a "permaneceu" failure can be classified
function describeSurvivors(pids, rootPid) {
  return new Promise(res => require('child_process').execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
    `Get-CimInstance Win32_Process | Where-Object { @(${pids.join(',')}) -contains $_.ProcessId } | ForEach-Object { "$($_.ProcessId) $($_.Name) parent=$($_.ParentProcessId) created=$($_.CreationDate.ToString('HH:mm:ss.fff')) cmd=$(([string]$_.CommandLine).Substring(0, [Math]::Min(120, ([string]$_.CommandLine).Length)))" }`],
    { windowsHide: true }, (e, out) => { console.log(`survivors (root pid ${rootPid}, now ${new Date().toISOString()}):
${String(out).trim() || '(none by the time of the query)'}`); res() }))
}

async function assertExit(a, action) {
  const pids = await processTree(a.proc.pid)
  assert.ok(pids.length >= 2, `a árvore de processos do Spark deveria incluir o WebView2: ${pids}`) // not in the original: guards that the pid list below is not vacuous
  const saved = fs.readFileSync(path.join(a.dir, 'spark.json'), 'utf8')
  let timeout
  const exited = new Promise((resolve, reject) => {
    timeout = setTimeout(() => reject(new Error(`${action}: o Spark não encerrou`)), 10000)
    a.proc.once('exit', (code, signal) => resolve({ code, signal }))
  })
  try {
    if (action === 'native') { await sleep(100); const r = await nativeClose(a.proc.pid); if (r !== 'True') { console.log(`(nativeClose devolveu "${r}"; usando ctl close)`); await a.ctl({ cmd: 'close' }) } }
    else await a.ui(`setTimeout(() => document.getElementById('${action}').click(), 100)`)
    assert.deepStrictEqual(await exited, { code: 0, signal: null }, 'saída normal, sem matar o processo')
    await sleep(1000)
    try { for (const pid of pids) assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' }, `processo ${pid} permaneceu em segundo plano`) }
    catch (e) { await describeSurvivors(pids, a.proc.pid); throw e } // assertion unchanged; diagnostics only (tells a lingering child from a reused pid)
    assert.strictEqual(fs.readFileSync(path.join(a.dir, 'spark.json'), 'utf8'), saved, 'sair não altera preferências, métricas nem a espera salva')
    console.log(`ok ${action}: encerrou normalmente, nenhum processo restante, configurações preservadas`)
  } finally { clearTimeout(timeout) }
}

const normal = s => !s.min && s.visivel && s.x > -10000
const minimizada = s => s.min && s.x > -10000 // minimizada de verdade (o Electron diz isVisible=false nesse estado), não na bandeja
const bandeja = s => !s.min && s.x <= -10000

;(async () => {
  let a, fixturePath
  try {
    // Fixture for the integration block below (the original installed it with protocol.handle after launch).
    fixturePath = path.join(os.tmpdir(), `spark-min-fixture-${process.pid}.html`)
    fs.writeFileSync(fixturePath, fs.readFileSync(path.join(ROOT, 'rust', 'app', 'tests', 'fixtures', 'fake-tinder.html'), 'utf8').replace('location.search', JSON.stringify('?offer=initial')))

    // 1. minimizar/restaurar 3x pelo botão amarelo
    a = await launch({}, [], { fakePage: fixturePath })
    assert.ok(normal(await a.info()), 'aberto à mão sem "Iniciar minimizado" deveria mostrar a janela')
    const ui = code => a.ui(code)
    const options = await ui('[...document.getElementById("retryMin").options].map(o => Number(o.value))')
    assert.deepStrictEqual(options, [15, 30, 60, 120, 360])
    for (const min of options) {
      await ui(`document.getElementById('retryMin').value = '${min}'; document.getElementById('retryMin').dispatchEvent(new Event('change'))`)
      await sleep(100)
      assert.strictEqual(JSON.parse(fs.readFileSync(path.join(a.dir, 'spark.json'), 'utf8')).retryMin, min)
      assert.strictEqual(await ui('Number(document.getElementById("retryMin").value)'), min)
    }
    console.log('ok seletor de intervalo: cinco opções, IPC e gravação reais')

    // Fluxo completo com a página de teste no navegador real, sem acessar uma conta do Tinder.
    await a.ctl({ cmd: 'tinder-load', url: 'https://tinder.com/app/recs' }) // the fixture is served by the host (SPARK_TEST_FAKE_PAGE)
    await ui("spark.set('retryMin', 30); spark.toggle(true)")
    await sleep(500)
    const state = await ui('spark.state()')
    assert.strictEqual(state.status.code, 'waiting')
    assert.strictEqual(state.status.text.split(' · ')[0], 'Likes esgotados')
    assert.ok(state.status.next > Date.now() + 29 * 60_000)
    assert.strictEqual(state.likesTotal, 0)
    await ui('spark.check()'); await sleep(500)
    const checked = await ui('spark.state()')
    assert.ok(checked.likesWaitFrom > state.likesWaitFrom, 'tentativa manual voltou a detectar a oferta')
    assert.strictEqual(checked.likesTotal, 0)
    await ui('spark.toggle(false)'); await sleep(100)
    assert.strictEqual((await ui('spark.state()')).status.code, 'paused')
    await ui('spark.toggle(true)'); await sleep(100)
    assert.strictEqual((await ui('spark.state()')).status.next, checked.status.next)
    await a.ctl({ cmd: 'capture', path: path.join(RESULTS, 'intervalo-interface.png') })
    console.log('ok integração real: motor detecta oferta → espera de 30 min → tentativa manual → nova espera → pausa/retomada')
    for (let i = 1; i <= 3; i++) {
      await ui("document.getElementById('wMin').click()")
      await sleep(2000)
      const m = await a.info()
      assert.ok(minimizada(m), `ciclo ${i}: depois de minimizar a janela não ficou minimizada: ${JSON.stringify(m)}`)
      await sleep(3000)
      assert.ok((await a.info()).min, `ciclo ${i}: a janela saiu sozinha do minimizado`)
      await a.ctl({ cmd: 'restore' })
      await sleep(1500)
      const r = await a.info()
      assert.ok(normal(r), `ciclo ${i}: restaurar não trouxe a janela de volta: ${JSON.stringify(r)}`)
      console.log(`ok ciclo ${i}: minimizou (ficou minimizada 5s) e restaurou`)
    }
    // minimizada → ícone da bandeja / abrir o atalho de novo (second-instance) tem que trazer a janela
    await a.ctl({ cmd: 'minimize' }); await sleep(1500)
    await a.ctl({ cmd: 'second-instance' })
    await sleep(1500)
    assert.ok(normal(await a.info()), 'abrir de novo não restaurou a janela minimizada')
    console.log('ok minimizada → abrir pelo atalho/bandeja restaura')

    // 2. o botão "Iniciar minimizado" nos Ajustes salva a opção
    await ui("document.getElementById('startMinimized').click()")
    await sleep(1000)
    assert.strictEqual(JSON.parse(fs.readFileSync(path.join(a.dir, 'spark.json'), 'utf8')).startMinimized, true)
    console.log('ok botão "Iniciar minimizado" salva a opção')

    // A tela tem que mostrar o status atual, não o anterior.
    await ui("spark.toggle(true)")
    await sleep(1000)
    const [tela, atual] = await ui("spark.state().then(s => [document.getElementById('status').textContent, s.status.text.split(' · ')[0]])")
    assert.strictEqual(tela, atual, `a tela mostra "${tela}" mas o status atual é "${atual}"`)
    console.log(`ok tela mostra o status atual ("${atual}")`)
    // original: capturePage({ x: 0, y: 0, width: 340, height: 860 }) — no crop in the control port; the whole window is saved (screenshot only, no assertion)
    await a.ctl({ cmd: 'capture', path: path.join(RESULTS, 'sair-interface.png') })
    await assertExit(a, 'quit')
    await a.close(); a = null

    a = await launch({ enabled: true, retryMin: 360, likesWaitFrom: Date.now() }, [], { offline: true })
    const waiting = await a.ui("spark.state().then(s => ({ text: document.getElementById('status').textContent, countdown: document.getElementById('sub').textContent, value: document.getElementById('retryMin').value, next: s.status.next }))")
    assert.strictEqual(waiting.text, 'Likes esgotados')
    assert.strictEqual(waiting.value, '360')
    assert.match(waiting.countdown, /^5:59:\d{2}$/)
    assert.ok(waiting.next > Date.now())
    await a.ctl({ cmd: 'size', w: 1100, h: 700 })
    const layout = await a.ui("(() => { const select = document.getElementById('retryMin'), box = select.getBoundingClientRect(); return { width: select.clientWidth, scroll: select.scrollWidth, visible: box.right <= document.querySelector('aside').getBoundingClientRect().right } })()")
    assert.ok(layout.width > 50 && layout.scroll <= layout.width && layout.visible, JSON.stringify(layout))
    await a.ctl({ cmd: 'capture', path: path.join(RESULTS, 'intervalo-minimo.png') })
    console.log('ok reabertura real: espera de 6 h retomada, contagem com horas e seletor no tamanho mínimo')
    await assertExit(a, 'wClose')
    await a.close(); a = null

    // 3. como a janela abre em cada combinação (aberto à mão / com o Windows = --hidden)
    for (const [cfg, args, esperado, nome, txt] of [
      [{ startMinimized: true }, [], minimizada, 'minimizada', 'Iniciar minimizado + aberto à mão'],
      [{ startMinimized: true }, ['--hidden'], minimizada, 'minimizada', 'Iniciar minimizado + com o Windows'],
      [{ startMinimized: false }, ['--hidden'], bandeja, 'na bandeja', 'sem Iniciar minimizado + com o Windows'],
    ]) {
      a = await launch(cfg, args)
      const s = await a.info()
      assert.ok(esperado(s), `${txt}: deveria abrir ${nome}: ${JSON.stringify(s)}`)
      await sleep(4000)
      assert.ok(esperado(await a.info()), `${txt}: não continuou ${nome}`)
      await a.ctl({ cmd: 'second-instance' }) // ícone da bandeja / atalho
      await sleep(1500)
      assert.ok(normal(await a.info()), `${txt}: abrir pelo atalho não mostrou a janela`)
      console.log(`ok ${txt}: abriu ${nome}, ficou assim, e o atalho mostra a janela`)
      await a.close(); a = null
    }
    // 4. abrir sem internet (como logo depois de entrar no Windows): avisa e tenta de novo em 1 min, sem "dar likes"
    a = await launch({ enabled: true }, ['--hidden'], { offline: true }) // proxy inválido: sem rede só aqui (the test-hooks build forces the proxy everywhere; offline = no fake page)
    await sleep(6000)
    const [txt, likes] = await a.ui("spark.state().then(s => [s.status.text, s.likesToday])")
    assert.ok(/^Sem conexão com o Tinder · testo de novo às/.test(txt) && likes === 0, `sem internet: "${txt}", likes ${likes}`)
    console.log(`ok sem internet: "${txt}", nenhum like`)
    await assertExit(a, 'native')
    await a.close(); a = null
    console.log('MINIMIZAR OK')
    process.exitCode = 0
  } catch (e) { console.error('FALHOU:', e.message); process.exitCode = 1; if (a) await a.close() }
  finally { if (fixturePath) fs.rmSync(fixturePath, { force: true }) }
})()
