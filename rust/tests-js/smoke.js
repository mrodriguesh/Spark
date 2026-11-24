// node rust/tests-js/smoke.js — first end-to-end run of the Rust app (simulation: temp profile, offline, fake page).
// Automatic on, fake Tinder with limit=3 persisted across reloads: expect 3 likes, then "Likes esgotados".
const fs = require('fs'), os = require('os'), path = require('path'), assert = require('assert')
const { launch, until, sleep, processTree, ROOT } = require('./lib')

;(async () => {
  const page = path.join(os.tmpdir(), `spark-fake-${process.pid}.html`)
  fs.writeFileSync(page, fs.readFileSync(path.join(ROOT, 'rust', 'app', 'tests', 'fixtures', 'fake-tinder.html'), 'utf8').replace('location.search', JSON.stringify('?persist=1&limit=3&radar=1')))
  let a
  try {
    a = await launch({ enabled: true, autostart: false, skipFirst: true, retryMin: 15 }, [], { SPARK_TEST_FAKE_PAGE: page })
    const s = await until(async () => { const s = await a.ui('spark.state()'); return s.status.text.startsWith('Likes esgotados') && s }, 'likes esgotados', 90000)
    console.log('status:', s.status.text, '| likesToday', s.likesToday, '| total', s.likesTotal, '| next in', Math.round((s.status.next - Date.now()) / 60000), 'min')
    assert.strictEqual(s.likesToday, 3); assert.strictEqual(s.likesTotal, 3); assert.ok(s.likesWaitFrom > 0)
    const disk = JSON.parse(fs.readFileSync(path.join(a.dir, 'spark.json'), 'utf8'))
    assert.strictEqual(disk.likesTotal, 3)
    console.log('ui text:', await a.ui("document.getElementById('status').textContent"), '| countdown', await a.ui("document.getElementById('sub').textContent"))
    console.log('tinder log:', JSON.stringify(await a.tinder('log')))
    console.log('logs:', JSON.stringify(s.logs.slice(0, 6)))
    const shot = path.join(ROOT, 'rust', 'tests-js', 'results', 'smoke.png'); fs.mkdirSync(path.dirname(shot), { recursive: true })
    console.log('capture:', JSON.stringify(await a.ctl({ cmd: 'capture', path: shot })))
    const tree = await processTree(a.proc.pid)
    const exited = new Promise(r => a.proc.once('exit', (code, signal) => r({ code, signal })))
    await a.ui("document.getElementById('quit').click()")
    const r = await Promise.race([exited, sleep(10000).then(() => 'timeout')])
    assert.deepStrictEqual(r, { code: 0, signal: null }, 'Sair encerra com código 0')
    await sleep(2000)
    const alive = tree.filter(pid => { try { process.kill(pid, 0); return true } catch { return false } })
    assert.deepStrictEqual(alive, [], 'nenhum processo restante')
    console.log(`SMOKE OK (${tree.length} processes ended)`)
  } catch (e) { console.error('FALHOU:', e.message); process.exitCode = 1 }
  finally { if (a) await a.close(); fs.rmSync(page, { force: true }) }
})()
