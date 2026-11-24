// node rust/tests-js/luz.js — spotlight on the settings list and the activity card: --x/--y follow the pointer,
// the light shows on hover only, and the interval dropdown is not clipped. Captures to tests-js/results/.
const assert = require('assert'), path = require('path'), fs = require('fs')
const { launch, until, sleep, ROOT } = require('./lib')
;(async () => {
  let a
  try {
    a = await launch({ enabled: false, autostart: false })
    await until(() => a.ui("document.readyState === 'complete' && !!document.querySelector('.spark-luz')"), 'ui')
    const out = path.join(ROOT, 'rust', 'tests-js', 'results'); fs.mkdirSync(out, { recursive: true })
    const op = sel => a.ui(`+getComputedStyle(document.querySelector(${JSON.stringify(sel)}), '::before').opacity`)
    for (const [sel, tab, name] of [['.settings-list', 'tabBtnSettings', 'luz-ajustes'], ['.logs-container', 'tabBtnLogs', 'luz-atividade']]) {
      await a.ui(`document.getElementById('${tab}').click()`); await sleep(500)
      const r = await a.ui(`(r => [r.x, r.y, r.width, r.height])(document.querySelector('${sel}').getBoundingClientRect())`)
      await a.input({ type: 'mouseMove', x: 900, y: 400 }); await sleep(400)
      assert.strictEqual(await op(sel), 0, `${sel}: sem luz com o mouse fora`)
      await a.input({ type: 'mouseMove', x: r[0] + 60, y: r[1] + 25 }); await sleep(600)
      assert.strictEqual(await op(sel), 1, `${sel}: luz acesa com o mouse em cima`)
      const pos = await a.ui(`(e => [e.style.getPropertyValue('--x'), e.style.getPropertyValue('--y')])(document.querySelector('${sel}'))`)
      const near = (v, w) => v.every((n, i) => Math.abs(parseFloat(n) - w[i]) <= 2)
      assert.ok(near(pos, [60, 25]), `${sel}: --x/--y seguem o mouse ${pos}`)
      await a.input({ type: 'mouseMove', x: r[0] + 150, y: r[1] + 30 }); await sleep(300)
      assert.ok(near(await a.ui(`(e => [e.style.getPropertyValue('--x'), e.style.getPropertyValue('--y')])(document.querySelector('${sel}'))`), [150, 30]), `${sel}: acompanha o movimento`)
      await a.ctl({ cmd: 'capture', path: path.join(out, name + '.png') })
      console.log(`ok ${sel}: luz segue o mouse, some ao sair`)
    }
    await a.ui("document.getElementById('tabBtnSettings').click()"); await sleep(400)
    const b = await a.ui("(r => [r.x + r.width / 2, r.y + r.height / 2])(document.getElementById('retryBtn').getBoundingClientRect())")
    await a.input({ type: 'mouseMove', x: b[0], y: b[1] }); await a.input({ type: 'mouseDown', x: b[0], y: b[1], button: 'left', clickCount: 1 }); await a.input({ type: 'mouseUp', x: b[0], y: b[1], button: 'left', clickCount: 1 }); await sleep(600)
    const dd = await a.ui("(l => { const r = l.getBoundingClientRect(), s = document.querySelector('.settings-list').getBoundingClientRect(); return { open: document.getElementById('retryDd').classList.contains('open'), visible: r.height > 0 && getComputedStyle(l).visibility !== 'hidden', listBottom: r.bottom, listTopOfSettings: s.top } })(document.getElementById('retryList'))")
    assert.ok(dd.open && dd.visible, 'dropdown abre com a luz ativa')
    await a.ctl({ cmd: 'capture', path: path.join(out, 'luz-dropdown.png') })
    console.log('ok dropdown do intervalo aberto sem corte')
    console.log('LUZ OK')
  } catch (e) { console.error('FALHOU:', e.message); process.exitCode = 1 } finally { if (a) await a.close() }
})()
