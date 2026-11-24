// node rust/tests-js/cursor.js — copy of test/cursor.js retargeted at the Rust app. Same assertions and messages;
// only the way the app is reached changed (rust/tests-js/lib.js):
//   Electron: spawn(electron, [root, --user-data-dir, --inspect=9342, --proxy-server=...]) + main-process CDP (BrowserWindow, webContents)
//   Rust:     spawn(Spark.exe, [--user-data-dir]) ; ui() = Runtime.evaluate in the UI page over WebView2's DevTools port,
//             input() = CDP Input.dispatchMouseEvent/KeyEvent on that page (Electron sendInputEvent equivalent),
//             window.setSize(w, h) = ctl({ cmd: 'size', w, h }) on the test-hooks control port.
// The test-hooks build forces an invalid proxy itself, so '--proxy-server=http://127.0.0.1:9' has no equivalent argument (nothing reaches the internet either way).
// Original header: o cursor personalizado (ponto + anel) no Spark de verdade; perfil temporário, nada sai para a internet.
const assert = require('assert')
const fs = require('fs')
const { launch, sleep } = require('./lib')

let ui, input, ctl // set after launch (the originals were module-level helpers over a fixed --inspect port)
async function until(fn, label, ms = 60000) { const end = Date.now() + ms; while (Date.now() < end) { try { const v = await fn(); if (v) return v } catch {} await sleep(300) } throw new Error('Tempo esgotado: ' + label) }
const move = async (x, y) => { await input({ type: 'mouseMove', x, y }); await sleep(150); await until(async () => { const s = await estado(); return Math.hypot(s.ring.x - s.dot.x, s.ring.y - s.dot.y) < 0.5 && s.dot.op > 0.99 }, 'anel alcançar o ponto', 3000) } // o anel tem inércia e o cursor aparece em fade: espera

// estado do cursor: centro (em px da janela) do ponto e do anel, opacidade, hover, escala e se o cursor nativo está escondido
const estado = () => ui(`(() => {
  const c = id => { const el = document.getElementById(id), r = el.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2, op: +getComputedStyle(el).opacity } }
  return { dot: c('cursorPonto'), ring: c('cursorAnel'), hov: document.getElementById('cursorPonto').classList.contains('hovering'),
    scale: document.getElementById('cursorAnel').style.scale, nativoOculto: document.documentElement.classList.contains('js-cursor-on') }
})()`)
const perto = (a, b, msg) => assert.ok(Math.abs(a - b) <= 1, `${msg}: ${a} ≠ ${b}`)

;(async () => {
  let a
  try {
    a = await launch({ enabled: false, autostart: false })
    ;({ ui, input, ctl } = a)
    await until(() => ui(`!!document.getElementById('cursorAnel') && document.readyState === 'complete'`), 'janela do Spark')
    await sleep(1500)

    // 1. a causa do bug: o cursor não pode morar dentro de um elemento com backdrop-filter/overflow
    assert.strictEqual(await ui(`document.getElementById('sparkCursor').parentElement === document.body`), true, 'cursor direto no <body>')
    assert.strictEqual(await ui(`!!document.getElementById('cursorPonto').closest('aside')`), false, 'cursor fora do <aside>')
    await input({ type: 'mouseMove', x: 800, y: 300 }); await sleep(600) // o mouse de verdade pode estar sobre a janela: começa fora da lateral
    assert.strictEqual((await estado()).dot.op, 0, 'escondido com o mouse fora da lateral')
    console.log('ok estrutura: cursor no <body>, fora do <aside>, escondido enquanto o mouse está fora da lateral')

    // 2. posição exata, com a lateral no topo e rolada (foi aí que ele se desalinhava)
    await ctl({ cmd: 'size', w: 1320, h: 720 }); await sleep(300) // janela baixa (mínimo 700): a lateral precisa ter o que rolar
    await ui(`document.getElementById('tabBtnSettings').click()`)
    for (const rolar of [0, 289]) {
      await ui(`document.querySelector('aside').scrollTop = ${rolar}`); await sleep(200)
      const scroll = await ui(`document.querySelector('aside').scrollTop`)
      assert.ok(rolar === 0 ? scroll === 0 : scroll > 40, `lateral rolada (scrollTop ${scroll})`) // > 40: o desalinhamento seria do tamanho da rolagem
      for (const [x, y] of [[120, 300], [200, 480], [250, 700], [60, 150]]) {
        await move(x, y)
        const s = await estado()
        perto(s.dot.x, x, `ponto x (scrollTop ${scroll})`); perto(s.dot.y, y, `ponto y (scrollTop ${scroll})`)
        perto(s.ring.x, x, `anel x (scrollTop ${scroll})`); perto(s.ring.y, y, `anel y (scrollTop ${scroll})`)
        assert.strictEqual(s.dot.op, 1); assert.strictEqual(s.nativoOculto, true, 'cursor nativo some enquanto o personalizado aparece')
      }
    }
    console.log('ok posição: ponto e anel exatamente sob o mouse em 8 pontos, com a lateral rolada ou não')

    // 3. hover sobre algo clicável, e rolar sem mexer o mouse troca o que está embaixo dele
    await ui(`document.querySelector('.switch').scrollIntoView({ block: 'center' })`); await sleep(200)
    const sw2 = JSON.parse(await ui(`(() => { const b = document.querySelector('.switch').getBoundingClientRect(); return JSON.stringify({ x: b.x + b.width / 2, y: b.y + b.height / 2 }) })()`))
    await move(sw2.x, sw2.y)
    let s = await estado(); assert.strictEqual(s.hov, true, 'hover no interruptor'); assert.strictEqual(s.scale, '1.3')
    await ui(`document.querySelector('aside').scrollTop += 220`); await sleep(300) // rola 220 px com o mouse parado
    const sob = await ui(`(() => { const e = document.elementFromPoint(${sw2.x}, ${sw2.y}); return !!e.closest('button, input, select, a, [role="button"], [role="switch"], .tab-pill, .help, .switch') })()`)
    s = await estado(); assert.strictEqual(s.hov, sob, 'hover reavaliado ao rolar com o mouse parado')
    console.log(`ok hover: interruptor acende o cursor (escala 1.3) e a rolagem com o mouse parado o reavalia (agora ${sob ? 'sobre' : 'fora de'} clicável)`)
    await ui(`document.querySelector('aside').scrollTop = 0`); await sleep(200) // cartão de métrica não é clicável: o cursor não pode se acender como se fosse
    const card = JSON.parse(await ui(`(() => { const b = document.querySelector('.metric-matches').getBoundingClientRect(); return JSON.stringify({ x: b.x + 20, y: b.y + b.height - 30 }) })()`))
    await move(card.x, card.y)
    s = await estado(); assert.strictEqual(s.hov, false, 'cartão de métrica não acende o cursor'); assert.strictEqual(s.scale, '1')
    console.log('ok hover: cartão de métrica (não clicável) mantém o cursor normal')

    // 4. clique encolhe o anel; soltar devolve
    await move(sw2.x, 400)
    await input({ type: 'mouseDown', x: sw2.x, y: 400, button: 'left', clickCount: 1 }); await sleep(250)
    assert.strictEqual((await estado()).scale, '0.75', 'anel encolhe ao clicar')
    await input({ type: 'mouseUp', x: sw2.x, y: 400, button: 'left', clickCount: 1 }); await sleep(250)
    assert.notStrictEqual((await estado()).scale, '0.75', 'anel volta ao soltar')
    console.log('ok clique: anel encolhe (0.75) ao apertar e volta ao soltar')

    // 5. seletor de intervalo (lista própria, não a nativa): o cursor personalizado segue visível, a lista abre/fecha e grava
    await ui(`document.getElementById('retryBtn').scrollIntoView({ block: 'center' })`); await sleep(200)
    const centro = sel => ui(`(() => { const b = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return JSON.stringify({ x: b.x + b.width / 2, y: b.y + b.height / 2 }) })()`).then(JSON.parse)
    const dd = () => ui(`JSON.stringify({ aberto: document.getElementById('retryDd').classList.contains('open'), val: document.getElementById('retryMin').value, txt: document.getElementById('retryTxt').textContent, kb: document.getElementById('retryDd').hasAttribute('data-kb') })`).then(JSON.parse)
    const btn = await centro('#retryBtn')
    await move(btn.x, btn.y); assert.strictEqual((await estado()).dot.op, 1); assert.strictEqual((await estado()).hov, true, 'botão do seletor acende o cursor')
    await input({ type: 'mouseDown', x: btn.x, y: btn.y, button: 'left', clickCount: 1 }); await input({ type: 'mouseUp', x: btn.x, y: btn.y, button: 'left', clickCount: 1 }); await sleep(300)
    assert.strictEqual((await dd()).aberto, true, 'clique abre a lista'); assert.strictEqual((await dd()).kb, false)
    const opt = await centro('#retryOpt3')
    await move(opt.x, opt.y); s = await estado(); assert.strictEqual(s.dot.op, 1, 'cursor personalizado segue visível com a lista aberta'); assert.strictEqual(s.hov, true, 'opção acende o cursor')
    await input({ type: 'mouseDown', x: opt.x, y: opt.y, button: 'left', clickCount: 1 }); await input({ type: 'mouseUp', x: opt.x, y: opt.y, button: 'left', clickCount: 1 }); await sleep(300)
    let d = await dd(); assert.deepStrictEqual([d.aberto, d.val, d.txt], [false, '120', '2 horas'], 'escolher fecha, troca o valor e o texto')
    assert.strictEqual(await ui(`spark.state().then(s => s.retryMin)`), 120, 'escolha gravada pelo IPC')
    // teclado: Enter/Espaço (click com detail 0) abre sem animação, setas movem, Enter escolhe, Esc fecha sem escolher, clique fora fecha
    await ui(`document.getElementById('retryBtn').focus(); document.getElementById('retryBtn').click()`); await sleep(100)
    d = await dd(); assert.deepStrictEqual([d.aberto, d.kb], [true, true], 'teclado abre sem animação')
    await input({ type: 'keyDown', keyCode: 'Down' }); await input({ type: 'keyUp', keyCode: 'Down' }); await sleep(80)
    await input({ type: 'keyDown', keyCode: 'Return' }); await input({ type: 'keyUp', keyCode: 'Return' }); await sleep(200)
    d = await dd(); assert.deepStrictEqual([d.aberto, d.val], [false, '360'], 'seta + Enter escolhem'); assert.strictEqual(await ui(`document.activeElement.id`), 'retryBtn', 'foco volta ao botão')
    await ui(`document.getElementById('retryBtn').click()`); await sleep(100)
    await input({ type: 'keyDown', keyCode: 'Up' }); await input({ type: 'keyUp', keyCode: 'Up' }); await input({ type: 'keyDown', keyCode: 'Escape' }); await input({ type: 'keyUp', keyCode: 'Escape' }); await sleep(150)
    d = await dd(); assert.deepStrictEqual([d.aberto, d.val], [false, '360'], 'Esc fecha sem escolher')
    await input({ type: 'mouseMove', x: btn.x, y: btn.y }); await input({ type: 'mouseDown', x: btn.x, y: btn.y, button: 'left', clickCount: 1 }); await input({ type: 'mouseUp', x: btn.x, y: btn.y, button: 'left', clickCount: 1 }); await sleep(250)
    assert.strictEqual((await dd()).aberto, true)
    await input({ type: 'mouseMove', x: 200, y: 120 }); await input({ type: 'mouseDown', x: 200, y: 120, button: 'left', clickCount: 1 }); await input({ type: 'mouseUp', x: 200, y: 120, button: 'left', clickCount: 1 }); await sleep(250)
    assert.strictEqual((await dd()).aberto, false, 'clique fora fecha')
    console.log('ok seletor: lista própria abre/fecha, cursor personalizado segue visível, mouse e teclado escolhem e gravam')

    // 6. o mouse sai da lateral: some e devolve o cursor nativo
    await input({ type: 'mouseMove', x: 800, y: 300 }); await sleep(500)
    s = await estado(); assert.strictEqual(s.dot.op, 0); assert.strictEqual(s.ring.op, 0); assert.strictEqual(s.nativoOculto, false)
    console.log('ok saída: ao sair da lateral o personalizado some e o cursor nativo volta')

    // 7. layout na aba Atividade: o #sparkCursor (filho do body) abria uma 2ª linha no grid e deixava uma faixa vazia embaixo
    await ui(`document.getElementById('tabBtnLogs').click()`)
    const layout = () => ui(`(() => { const b = s => Math.round(document.querySelector(s).getBoundingClientRect().bottom), a = document.querySelector('aside')
      return JSON.stringify({ h: innerHeight, aside: b('aside'), main: b('main'), pane: b('#paneLogs'), rola: a.scrollHeight > a.clientHeight }) })()`).then(JSON.parse)
    for (const [w, h] of [[1320, 860], [1600, 1000]]) {
      await ctl({ cmd: 'size', w, h }); await sleep(500)
      const l = await layout()
      assert.strictEqual(l.aside, l.h, `lateral vai até o fim da janela (${h})`); assert.strictEqual(l.main, l.h, `navegador vai até o fim da janela (${h})`)
      assert.ok(!l.rola && l.pane >= l.h - 21, `Atividade ocupa a altura livre sem rolar a lateral (${JSON.stringify(l)})`)
    }
    console.log('ok Atividade: lateral e navegador até o fim da janela, lista ocupa a altura livre (860 e 1000 px)')

    // 8. movimento: indicador das abas desliza no clique e não anima pelo teclado; número que sobe e log novo animam uma vez
    const m = JSON.parse(await ui(`(async () => {
      const ind = () => document.getAnimations().filter(a => a.effect?.pseudoElement === '::before' && a.effect.target.classList.contains('segmented-nav')).length
      document.getElementById('tabBtnSettings').click(); const clique = ind()
      document.querySelector('aside').dataset.teclado = ''; document.getElementById('tabBtnLogs').click(); const teclado = ind()
      delete document.querySelector('aside').dataset.teclado
      const antes = document.getElementById('today').dataset.v; numero('today', Number(antes) + 1); const tick = document.getElementById('today').getAnimations().length
      numero('today', Number(antes)); const desceu = document.getElementById('today').getAnimations().filter(a => a.playState === 'running').length - tick
      const st = await spark.state(); st.logs = ['22/11, 17:50  teste de log novo', ...st.logs]; render(st)
      const novo = document.querySelectorAll('.log.novo').length; render(st)
      return JSON.stringify({ clique, teclado, tick, desceu, novo, depois: document.querySelectorAll('.log.novo').length })
    })()`))
    assert.ok(m.clique >= 1, 'indicador desliza no clique'); assert.strictEqual(m.teclado, 0, 'pelo teclado não anima')
    assert.strictEqual(m.tick, 1, 'número que sobe anima'); assert.ok(m.desceu <= 0, 'número que desce não ganha animação nova')
    assert.deepStrictEqual([m.novo, m.depois], [1, 0], 'só a linha recém-chegada entra animada, uma vez')
    console.log('ok movimento: abas (clique sim, teclado não), número que sobe e log novo animam uma vez')
    console.log('CURSOR OK')
  } catch (e) { console.error('FALHOU:', e.message); process.exitCode = 1 }
  finally { if (a) await a.close() }
})()
